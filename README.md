# gnomon

> **gnomon** /ˈnoʊmɒn/ — the part of a sundial that casts the shadow; an instrument
> that reads the time from the sky. Also: **GN**SS **mon**itor.

An NMEA 0183 monitor written in Rust, modelled on `gpsmon` from gpsd.
It reads from gpsd over the network, from a TCP port carrying raw NMEA
(a serial-to-TCP bridge, the NMEA output of an NTP appliance, ...), or from a
receiver attached directly to a serial port, and shows satellites, the fix,
per-sentence statistics and the raw data stream.

No external crates (std only). Builds with plain `cargo build` on macOS, Linux
and the BSDs.

## Install

```sh
cargo install gnomon-gnss
```

The crate is published as `gnomon-gnss` (the name `gnomon` is taken on
crates.io); the installed command is `gnomon`.

## Build from source

```sh
cargo build --release
./target/release/gnomon ntp.example.net:4001
```

For a Raspberry Pi and similar, build on the target itself, or cross-compile,
e.g. `cargo zigbuild --release --target aarch64-unknown-linux-gnu` from macOS.
Requires Rust 1.80 or later.

## Usage

```
gnomon [options] [target]

target (default localhost:2947):
  host[:port]            auto-detect gpsd JSON or raw NMEA
  host:port:/dev/ttyX    gpsd, watch only that device
  [ipv6]:port            IPv6 literal
  tcp://host:port        raw NMEA only (never writes to the peer)
  gpsd://host[:port]     gpsd only
  /dev/ttyXXX            serial port (on macOS use /dev/cu.*)

options:
  -r, --raw              treat the target as raw NMEA (same as tcp://)
  -g, --gpsd             treat the target as gpsd
  -s, --speed BPS        serial speed (default: auto-detect)
  -l, --log FILE         append every received byte to FILE
  -f, --replay FILE      play back a capture, paced at 1 epoch/s by sentence time
                         ('-' reads stdin, unpaced)
  -a, --ascii            ASCII box drawing (for terminals treating ambiguous
                         width as double)
  -j, --json             show gpsd JSON in the raw window from the start
      --snapshot SECS    no TUI: receive for SECS seconds, print one screen as
                         text and exit
      --size WxH         screen size for --snapshot (default 100x40)
```

Keys: `q` quit / `p` (or space) pause the raw window / `c` clear statistics /
`j` toggle JSON lines

## Protocol auto-detection

Decided by the first non-whitespace byte received after connecting:

- `{` → gpsd (VERSION banner). Sends
  `?WATCH={"enable":true,"json":true,"raw":1,"pps":true}`
- `$` / `!` → raw NMEA. Nothing is ever written to the peer.
- Nothing received: only on port 2947 is the peer assumed to be gpsd after
  2 seconds and sent a WATCH. On any other port gnomon keeps waiting without
  writing, so that no junk ever reaches a GNSS receiver's serial input.

After 15 seconds without data, or on disconnect, it reconnects automatically
(back-off 1→2→4→8 s, reset once data is received).

## Serial input

```sh
gnomon /dev/ttyAMA0              # auto-detect speed
gnomon -s 115200 /dev/ttyUSB0    # fixed speed
gnomon /dev/cu.usbserial-110     # macOS
```

- 8N1, raw, no flow control, **`CLOCAL`** (carrier detect ignored, the
  equivalent of kermit's `SET CARRIER-WATCH OFF`), `HUPCL` off, VMIN=0/VTIME=5.
  termios is driven directly (`src/tty.rs`, with the struct defined separately
  for Linux, macOS and the BSDs), and `CLOCAL` and the speed are read back and
  verified after setting.
- Opened with `O_NONBLOCK` so that open(2) itself never waits for DCD, then
  `CLOCAL` is set, then the descriptor is switched back to blocking. The
  connection status shows the DCD state (`DCD off (ignored)` etc.).
- Opened exclusively with `TIOCEXCL` (gpsd and others can no longer open the
  port; a process that already has it open is not evicted).
- Speed auto-detection tries 9600, 115200, 38400, 4800, 19200, 57600, 230400,
  2.5 s each, and picks the first speed that yields two sentences with valid
  checksums. On reconnect, the last good speed is tried first.
- The input buffer is discarded with `tcflush` right after opening and after
  every speed change, so stale lines are never shown with fresh timestamps.
- Unplugging a USB serial adapter is detected as a read error; gnomon keeps
  trying to reopen the port.
- Nothing is ever written to the receiver.
- **Do not open a device that gpsd is using.** Both would read from it, steal
  bytes from each other, and both would see corrupted data. gnomon also
  changes the line settings, so gpsd may need a restart afterwards.
- `Lag (tx start)`: on a serial port, the time each line's first byte went out
  on the wire is estimated as "chunk arrival time − bytes that followed ×
  10 bits / speed". The value is therefore close to the offset between the
  moment the receiver started sending the epoch and the UTC second (the
  latency timer of a USB serial adapter, 16 ms by default for FTDI, still
  remains). Estimated line times are clamped to be monotonic.
- **USB CDC-ACM devices** (macOS `cu.usbmodem*`, Linux `ttyACM*`; u-blox and
  other receivers with native USB) have only a nominal baud rate and deliver
  data in USB transfers, so no back-dating is done and the arrival time is used
  as-is. The header shows `serial USB CDC` and Lag shows `(host-fix)`.

## Screen

- **Satellites**: system / PRN / elevation / azimuth / C/N0 / used in fix.
  With NMEA 4.10+, C/N0 per GSV signal ID is shown separated by `/`.
  The system is taken from the GSA system ID (field 18) when present, otherwise
  from the talker, and from the PRN range for the GN talker. SBAS (33-64,
  120-158) and QZSS (193-202) reported under the GPS talker are shown as their
  own systems.
- **Fix**: merged from RMC/GGA/GNS/GLL/ZDA/VTG/GSA/GST/TXT.
  - `Lag (host-fix)`: host receive time of the first time-bearing sentence of
    each epoch minus the UTC time in that sentence. The sum of receiver output
    delay, network delay and host clock error; useful for checking NMEA output
    timing.
- **Sentences**: count and output interval (Intvl) per address. The interval is
  an EWMA of arrival intervals measured per NMEA epoch (a change of the time in
  RMC/GGA etc.), so sentences such as GSV that arrive several times per epoch
  count once. When a sentence is missing for 3× its interval (at least 3 s),
  the elapsed seconds are shown in yellow as `12s!`. Sentences that never
  established an interval (seen in only one epoch, such as start-up GPTXT
  banners) are not treated as missing. Shown in red if any checksum failed.
- **gpsd** (gpsd connections only): VERSION, DEVICES, TPV, and
  `real − clock` from PPS / TOFF (PPS in µs, TOFF in ms; positive means the
  system clock is behind).
- **Raw data**: raw lines with host receive time (UTC, ms). Lines without a
  checksum are yellow, lines with a bad checksum red.

## Differences from gpsd's gpsmon

- NMEA 0183 only. No per-driver panels for binary protocols such as UBX, SiRF
  or TSIP.
- No commands are sent to the device (no receiver baud-rate changes, output
  configuration, etc.).
- With gpsd, `raw:1` is used, so binary protocols only appear as hex lines in
  the raw window.

## Acknowledgements

gnomon would not exist without [gpsd](https://gpsd.io/). For three decades
gpsd has been the quiet, dependable piece that sits between GNSS receivers and
everything else, and `gpsmon` in particular set the pattern this tool follows:
a plain terminal screen that shows exactly what the receiver is saying.

Our sincere thanks and respect go to Remco Treffkorn, who started gpsd in 1995,
and Derrick J. Brashear; to Russ Nelson, who maintained it in its early years;
to Eric S. Raymond, who rewrote it in 2004–2005 and designed the JSON protocol
that gnomon speaks; to Chris Kuethe and Gary E. Miller; and to the many other
contributors who have kept gpsd running on countless receivers, NTP servers
and embedded systems around the world. gpsd's documentation, notably
*NMEA Revealed*, remains an invaluable reference for anyone who has to make
sense of NMEA 0183 as it is actually spoken by real hardware.

Any mistakes in gnomon are entirely our own.

## License

BSD-2-Clause, to match gpsd. The code is a new implementation modelled on the
behaviour of gpsmon and contains no gpsd source code.
