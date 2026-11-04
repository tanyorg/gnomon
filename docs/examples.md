# Field notes

Worked examples of diagnosing real problems with gnomon and metronomon. The
screen references are [gnomon.md](gnomon.md) and
[metronomon.md](metronomon.md); this page shows how the values were read in
practice.

## Raspberry Pi: under-voltage shows up as PPS blocks

### Symptom

A GNSS receiver feeds PPS into a Raspberry Pi GPIO (`pps-gpio`) and chronyd
disciplines the clock from it. metronomon's sparkline shows **blocks of a few
seconds** where the offset jumps to +3 to +5 µs, stays almost flat, and drops
back. The blocks come at irregular intervals of roughly 10 to 30 s.

```
     +3.610                    █▃          █                 ▃▂▄           ▅      ▅▆▆
          0               ▇▁   ██▇         █          ▂  ▆   ███   ▂  ▄    █  ▁▁ ▂███▁
                            ▂▄▅   ▄▄▃▃▂▂▃▄▁ ▅▃▄▄▄▅▄▅▄▅ ▆▅ ▄▃▇   ▄▄▆ ▃▃ ▅▃▁▅▅▆▃▄▆ ▅▄▄ ▅▇
     -3.610
```

What the numbers look like:

- sd is 1 to 2 µs, larger than the second-to-second jitter would suggest.
- drms / sd is below 1.41 (positive lag-1 correlation; see *Reading drms
  against sd* in [metronomon.md](metronomon.md)). Over 10 minutes it can
  approach 1.0, which looks like slow wander, but the sparkline shows steps,
  not drift.
- The baseline between blocks sits slightly **below** zero: the servo centres
  the mean of everything, blocks included.
- `missing`, `extra` and `reader-gaps` stay at 0. The pulses are all there;
  only their timestamps move.

### Cause

When the 5 V input drops below the firmware's threshold (about 4.63 V on
recent models), the firmware throttles the ARM clock, on a Pi 3 from 1200 MHz
to 600 MHz. At the lower clock the path from the GPIO interrupt to the kernel
timestamp takes a few µs longer, so the pulse is stamped late and the offset
reads positive (system clock ahead) for as long as the throttling lasts.

### Diagnosis

`vcgencmd get_throttled` reports the firmware's state as a bit field:

| Bit | Mask | Meaning |
|---|---|---|
| 0 | `0x1` | Under-voltage now |
| 1 | `0x2` | ARM frequency capped now |
| 2 | `0x4` | Throttled now |
| 3 | `0x8` | Soft temperature limit active now |
| 16 | `0x10000` | Under-voltage has occurred since boot |
| 17 | `0x20000` | ARM frequency capping has occurred |
| 18 | `0x40000` | Throttling has occurred |
| 19 | `0x80000` | Soft temperature limit has occurred |

Anything other than `0x0` deserves a look. `0x50000` means under-voltage and
throttling have happened since boot; `0x50005` means both are happening right
now.

The kernel logs each episode:

```sh
dmesg -T | grep -iE "undervoltage|normalised"
```

```
hwmon hwmon1: Undervoltage detected!
hwmon hwmon1: Voltage normalised
```

To tie the blocks to the throttling, log the ARM clock once per second while
metronomon runs in another terminal, and compare the timestamps:

```sh
#!/usr/bin/env sh
# Log ARM clock and throttle state once per second (UTC) for comparison
# with metronomon's Recent pulses.
while :; do
    printf '%s %s %s\n' "$(date -u +%H:%M:%S)" \
        "$(vcgencmd measure_clock arm | cut -d= -f2)" \
        "$(vcgencmd get_throttled | cut -d= -f2)"
    sleep 1
done
```

### Pitfalls

- **The kernel does not see the throttling.** With `force_turbo=1` the
  cpufreq driver offers only the maximum frequency, so `scaling_cur_freq`
  and the governor always report full speed while the firmware runs the core
  at 600 MHz. Only `vcgencmd measure_clock arm` shows the real clock.
- **`performance` governor and `force_turbo=1` do not prevent it.** Under-
  voltage throttling overrides both.
- **Check the model, not the host name.** `/proc/device-tree/model` tells
  which board it is, and therefore which clock and power figures apply.

### Real case

A Raspberry Pi 3 Model B with a GT-502MGG-N on the GPIO header (UART for
NMEA, `pps-gpio` for PPS), chronyd disciplining from the PPS. The 5 V supply
reached the board through a thin USB cable; `dmesg` showed an under-voltage
episode lasting 4 to 7 s every 10 to 30 s, even with the CPU almost idle.

metronomon's Recent pulses next to the clock log for the same seconds:

| UTC | ARM clock | `get_throttled` | PPS offset |
|---|---|---|---|
| 07:44:37 | 600 MHz | `0x50005` | +2.906 µs |
| 07:44:38 | 600 MHz | `0x50005` | +3.185 µs |
| 07:44:39 | 600 MHz | `0x50005` | +3.208 µs |
| 07:44:40 | 1200 MHz | `0x50000` | +0.210 µs |
| 07:44:41 | 1200 MHz | `0x50000` | −0.184 µs |

The block starts with the throttling and ends in the first second after the
clock recovers. A once-per-second `vcgencmd` poll can miss short episodes, so
not every block needs a matching 600 MHz line; compare with the `dmesg`
episodes as well.

Replacing the USB cable with a short, thicker one removed the problem:

| | Before | After (40 min) |
|---|---|---|
| Under-voltage | Every 10 to 30 s | None (`get_throttled` = `0x0`) |
| ARM clock | Switching 600 / 1200 MHz | 1200 MHz |
| 1 min sd | 1.17 µs | 0.66 to 0.87 µs |
| 1 min drms | 1.30 µs | 0.84 to 1.18 µs |
| Sparkline | Blocks of +3 µs | No blocks |

### Fix

- Use a supply rated 5.1 V (not 5.0 V) with enough current: 2.5 A for a
  Pi 3, 3 A (USB-C) for a Pi 4. The extra 0.1 V covers the drop in the cable
  and connector.
- Prefer a supply with an attached cable, or a short cable with thick power
  conductors. A thin cable can lose several hundred mV at 2 A.
- A GNSS module and active antenna powered from the GPIO header draw from
  the same 5 V input. If under-voltage persists, power them separately with a
  common ground.
- Put USB devices behind a self-powered hub. Plugging in a hub or a dongle can
  itself cause a burst of under-voltage episodes.
- After the change, check that `get_throttled` stays at `0x0` and that the
  blocks have gone from the sparkline.
