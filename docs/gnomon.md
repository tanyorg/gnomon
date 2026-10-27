# gnomon screen reference

A field-by-field guide to what gnomon shows and which NMEA 0183 sentence each
value comes from. The [README](../README.md) describes the screen as a whole;
this page is for looking things up.

## Satellites

| Column | Source | Meaning |
|---|---|---|
| Sy | talker, GSA system ID, PRN range | Constellation (see below) |
| PRN | GSV | Satellite number as given in NMEA (see *SBAS numbering*) |
| El / Az | GSV | Elevation and azimuth, degrees |
| SNR | GSV | C/N0 in dB-Hz. With NMEA 4.10+, one value per signal ID, separated by `/` |
| U | GSA | `Y` if the satellite is used in the fix |

The title `Satellites 16/30` reads *used / in view*.

### Constellations

| Sy | System |
|---|---|
| GP | GPS |
| GL | GLONASS |
| GA | Galileo |
| BD | BeiDou |
| QZ | QZSS (Michibiki) |
| IR | NavIC (IRNSS) |
| SB | SBAS (WAAS, EGNOS, MSAS, ...) |
| ?? | Could not be determined |

SBAS (NMEA 33-64, 120-158) and QZSS (193-202) reported under the GPS talker
are shown as their own systems.

### Colours

| Colour | Meaning |
|---|---|
| Green, bold | Used in the fix (listed in GSA) |
| Cyan | SBAS satellite being received (has an SNR) |
| Plain | Received but not used |
| Dim | Listed in GSV but no signal |

SBAS satellites mostly broadcast corrections rather than ranging signals, so
they are normally not listed in GSA and never turn green. When their
corrections are applied, the Fix box shows `Qual 2 DGPS` / `Mode diff`.

### SBAS numbering

NMEA 0183 has two-digit satellite fields, so SBAS PRNs 120-158 are sent as
33-64:

```
NMEA number = PRN - 87        e.g. NMEA 50 = PRN 137
```

## Fix

| Field | Example | Source | Meaning |
|---|---|---|---|
| Time | `22:22:24.000Z` | RMC / GGA / GNS / GLL / ZDA | UTC time of day reported by the receiver, to the millisecond |
| Date | `2026-10-26` | RMC or ZDA | GGA carries no date |
| Lat / Lon | | GGA / RMC / GLL / GNS | Converted from degrees-and-minutes to decimal degrees (7 decimals) |
| Alt | `41.4 m` | GGA altitude | Height above mean sea level (geoid), provided the receiver applies a geoid model |
| Sep | `27.3` | GGA geoid separation | Ellipsoid minus geoid. About 30 m in Japan. **0.0 usually means no geoid model**, and Alt is then ellipsoidal height |
| Fix | `3D` | GSA fix mode | `no fix`, `2D` (height assumed) or `3D` |
| RMC | `A(valid)` | RMC status | `A` usable, `V` warning |
| Qual | `2 DGPS` | GGA quality | See *Qual codes* |
| Mode | `diff` | RMC / GNS / GLL mode indicator (NMEA 2.3+) | See *Mode codes* |
| DOP | `P1.05 H0.74 V0.74` | GSA | Satellite geometry: position, horizontal, vertical. Lower is better (~1 excellent, 2-4 typical, >6 poor) |
| Used | `16 (GGA)` | GGA satellites in use | Counted by the receiver; may differ from the Satellites title, which counts GSA entries |
| Lag | `+370.9 ms (host-fix)` | first time-bearing sentence of each epoch | See *Lag* |
| Spd / Trk | `0.240 kn` / `244.2` | RMC or VTG | Speed (knots) and course (degrees true). A stationary receiver still shows some jitter |
| GST | | GST | Receiver's 1-sigma error estimate for latitude / longitude / altitude, m (shown only if sent) |
| MagV | | RMC | Magnetic variation (shown only if sent) |
| Diff | | GGA | Age of differential corrections and reference station ID (shown only if sent) |
| TXT | `ANTENNA OK` | TXT | Last text message from the receiver (shown only if sent) |

The `Fix ... RMC ...` line is **green** for 3D and `A`, **red** for `V` or no
fix, and **yellow** otherwise (2D etc.).

### Qual codes (GGA)

| Code | Label | Meaning |
|---|---|---|
| 0 | invalid | No fix |
| 1 | GPS | Standalone fix |
| 2 | DGPS | Differential (SBAS etc.) |
| 3 | PPS | PPS / precise mode |
| 4 | RTK fix | RTK fixed |
| 5 | RTK flt | RTK float |
| 6 | DR | Dead reckoning |
| 7 | manual | Manual input |
| 8 | sim | Simulation |

### Mode codes (RMC / GNS / GLL)

| Code | Label | Meaning |
|---|---|---|
| A | auto | Autonomous |
| D | diff | Differential |
| E | DR | Dead reckoning |
| F | RTKflt | RTK float |
| M | manual | Manual input |
| N | nofix | No fix |
| P | precise | Precise |
| R | RTKfix | RTK fixed |
| S | sim | Simulation |

Receivers differ in how they use the rarer codes; check the receiver's manual.

### Lag

Lag is specific to gnomon. For the first time-bearing sentence of each NMEA
epoch:

```
Lag = time the sentence reached the host - UTC time written in the sentence
```

It is the sum of the receiver's output delay, transport delay and host clock
error, and turns red above 1 s.

| Label | Used for | How the arrival time is taken |
|---|---|---|
| `host-fix` | gpsd, raw TCP, USB CDC serial | Arrival time as-is |
| `tx start` | Other serial ports | Back-dated to when the sentence's first byte went on the wire: chunk arrival time - following bytes x 10 bits / speed |

What remains after a fast link is the receiver's own processing time: for
example about 90 ms for an ATGM332D-based appliance and about 370 ms for a
GT-502MGG-N.

## Sentences

| Column | Meaning |
|---|---|
| Type | Talker + sentence (`GPGSV`, `GNRMC`, ...) |
| Count | Sentences received |
| Intvl | Output interval, EWMA per NMEA epoch (a sentence sent several times per epoch, such as GSV, counts once) |

A sentence missing for 3x its interval (at least 3 s) shows the elapsed time in
yellow, e.g. `12s!`. A type with checksum failures is shown in red.

## gpsd (gpsd connections only)

| Line | Meaning |
|---|---|
| version | gpsd release and protocol version |
| Dev | Device path, driver and speed from DEVICES |
| TPV | Time and mode of the latest TPV report, with `ept` (estimated time error) |
| PPS / TOFF | `real - clock` from gpsd's PPS (µs) and TOFF (ms) reports. Positive means the system clock is behind |

## Raw data

Every received line with its host receive time (UTC, ms). Lines without a
checksum are yellow; lines with a bad checksum are red. The footer counts
received lines (`rx`), bad checksums (`bad-ck`), lines without a checksum
(`no-ck`) and reconnects.
