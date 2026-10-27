# metronomon screen reference

How metronomon measures each pulse, what every value on its screen means and
how to read them. The [README](../README.md) covers installation and usage;
this page is for looking things up.

## How a pulse is measured

metronomon reads `/dev/ppsN` with `PPS_FETCH` (RFC 2783). Fetching is
**non-destructive**: chronyd or ntpd reading the same device see exactly the
same events, so metronomon can run next to them on a production server.

For each assert event:

```
second = the UTC second nearest to the assert timestamp
offset = assert timestamp - second
```

**Positive offset means the system clock was ahead** when the pulse arrived.

- If the system clock is disciplined from this same PPS (chrony `refclock
  PPS`, ntpd's PPS driver), offset is the residual of that servo: it should
  average zero, and its spread is the jitter of the whole chain (receiver,
  cable, interrupt latency, servo).
- If the clock is disciplined from something else, offset is the error of the
  system clock against this PPS.

### Interval and width

| Value | Meaning |
|---|---|
| `intvl` | Time from the previous kept pulse's assert to this one. `intvl - 1 s` is almost exactly the change of offset since the previous second (the difference is the clock's frequency error, typically tens of ns) |
| `width` | Clear minus assert of the same pulse, when the device also reports clear edges |
| `(no clear edges)` in the box title | The device says it can capture clear edges, but none has been seen, so there is no `width`. Common with pps-ldisc on a serial DCD line; harmless |

## Pulses that do not fit

Each pulse is compared with a **reference offset**: the median of the last 15
on-time pulses.

| Event | Rule |
|---|---|
| **on time** | Within ±100 ms of the reference. Counted in the statistics |
| **extra** | More than 100 ms from the reference: a glitch, ringing on the line, a second edge. Not counted in the statistics; logged in yellow |
| **missing** | A UTC second with no on-time pulse between two that have one. Logged in red, shown as `!` in the sparkline |
| **reader-gaps** | Kernel sequence numbers skipped: events the kernel saw but metronomon did not fetch in time (heavy load, a suspended terminal) |
| **re-baselined** | After a clock step every pulse looks off-time. Once 5 consecutive off-time pulses agree with each other (spread < 100 ms), metronomon takes them as the new reference, clears the history and stops counting them as extra |
| **timeouts** | `PPS_FETCH` waited 1.5 s or more without a pulse (the first wait after opening is ignored) |
| **NO PULSE for Ns** | No pulse for more than 2.5 s |

All counters are zero on a healthy source. Any missing, extra or reader-gap
event turns the counter line yellow.

## Statistics

Two rows, over the last 60 and the last 600 on-time pulses (1 and 10 minutes
at 1 PPS). Units switch automatically between ns, µs and ms.

| Column | Meaning |
|---|---|
| n | Pulses in the window |
| mean | Mean offset. About zero when the clock is disciplined from this PPS; a persistent non-zero mean means the servo follows another source or has an `offset` configured |
| sd | Standard deviation: total spread |
| min / max | Extremes. For Gaussian noise `max - min` is about 5-6 × sd; much more means occasional spikes |
| drms | RMS of the differences between consecutive seconds: second-to-second jitter |

### Reading drms against sd

For white noise, consecutive offsets are independent and `drms ≈ sd × √2`.
The ratio tells what kind of noise dominates:

| drms / sd | Lag-1 correlation | What it means |
|---|---|---|
| ≈ 1.41 | ≈ 0 | White noise: each second independent |
| > 1.41 | negative | Consecutive offsets **alternate** (zigzag): typical of a pulse source or interrupt path that dithers |
| < 1.41 | positive | The offset **wanders** slowly: servo, temperature, power supply, periodic load |

The lag-1 autocorrelation can be computed from the two columns:

```
r1 = 1 - drms² / (2 × sd²)
```

Example: sd 1.65 µs, drms 2.88 µs gives r1 ≈ -0.52, a clear zigzag.

## Sparkline

One bar per second, newest on the right, growing up for positive and down for
negative offsets from a zero line in the middle.

- **Scale**: ±(|median| + 4 × 1.4826 × MAD) of the shown offsets. 1.4826 × MAD
  is a robust estimate of sd, so a single transient does not flatten the rest.
  The labels on the left show the scale.
- **Yellow bar tip**: the offset is outside the scale and the bar is clipped.
- **Red `!`** on the zero line: no pulse in that second.

What patterns usually mean:

| Pattern | Likely cause |
|---|---|
| Bars alternating up and down | Zigzag (negative r1) |
| Long runs on the same side | Slow wander (positive r1) |
| One tall bar | A spike: one late interrupt, a missed timer |
| Blocks of a few seconds at regular or irregular intervals | An external disturbance: CPU frequency changes, under-voltage throttling, a periodic job |

## Status line

| Indication | Meaning |
|---|---|
| `OK` (green) | Pulses arriving |
| `opening` | Device being opened |
| `waiting for pulses` (yellow) | Device open, no pulse seen yet |
| `NO PULSE for Ns` (red) | No pulse for more than 2.5 s |
| `ERROR ...` (red) | Open or fetch failed. Permission errors come with a hint (see below) |

After the state: sequence number, last offset, `intvl` and `width`. On a narrow
terminal whole fields are dropped rather than cut: `width` first, then `seq`,
then `intvl`; the last offset is always shown.

## Recent pulses

A log of the latest pulses from all sources: sequence number, UTC second,
offset and interval. Red lines report pulses missing before this one; yellow
lines report extra pulses (not counted).

## Header

The host name (up to the first dot) and the system clock in UTC, so
screenshots from several machines can be told apart.

## Permissions

`/dev/ppsN` is often root-only (`crw------- root`). Use `sudo`, add yourself to
the device's group, or add a udev rule such as

```
KERNEL=="pps0", GROUP="dialout", MODE="0660"
```

The error message names the device's group when it has one.

## Simulated sources

For trying metronomon on any OS (macOS included) without hardware:

| Source | Behaviour |
|---|---|
| `sim:gpio` | A few µs of jitter |
| `sim:uart` | Sub-µs jitter, 100 ms pulse width |
| `sim:usb` | Sub-ms jitter of a USB serial adapter, occasional missing pulse |
| `sim:bad` | Missing and extra pulses |
