#!/usr/bin/env python3
# Mock NMEA sources for testing gnomon.
#   mockgps.py [--lat D] [--lon D] [--alt M] raw  PORT
#       raw NMEA over TCP (like a serial-over-TCP bridge)
#   mockgps.py [--lat D] [--lon D] [--alt M] gpsd PORT
#       fake gpsd: VERSION banner, waits for ?WATCH, then raw NMEA + TPV/PPS/TOFF
# The default position is the Royal Observatory, Greenwich (prime meridian).
import argparse, json, socket, sys, threading, time

# Decimal degrees / metres above the ellipsoid-referenced geoid.
POS = {"lat": 51.477811, "lon": -0.001475, "alt": 46.0, "sep": 45.9}


def set_position(lat, lon, alt=None):
    if not -90.0 <= lat <= 90.0 or not -180.0 <= lon <= 180.0:
        raise ValueError("latitude must be within +-90, longitude within +-180")
    POS["lat"], POS["lon"] = lat, lon
    if alt is not None:
        POS["alt"] = alt


def nmea_coord(deg, is_lat):
    """Decimal degrees -> ('ddmm.mmmm', 'N') / ('dddmm.mmmm', 'E')."""
    hemi = ("N" if deg >= 0 else "S") if is_lat else ("E" if deg >= 0 else "W")
    a = abs(deg)
    d = int(a)
    m = round((a - d) * 60.0, 4)
    if m >= 60.0:  # rounding carried into the next degree
        d, m = d + 1, 0.0
    width = 2 if is_lat else 3
    return f"{d:0{width}d}{m:07.4f}", hemi

def ck(body):
    c = 0
    for b in body.encode():
        c ^= b
    return "$%s*%02X" % (body, c)

def epoch(t, n):
    tm = time.gmtime(int(t))
    hms = time.strftime("%H%M%S", tm) + ".00"
    dmy = time.strftime("%d%m%y", tm)
    la, ns = nmea_coord(POS["lat"], True)
    lo, ew = nmea_coord(POS["lon"], False)
    alt, sep = POS["alt"], POS["sep"]
    out = [
        ck(f"GNRMC,{hms},A,{la},{ns},{lo},{ew},0.012,,{dmy},,,A,V"),
        ck(f"GNGGA,{hms},{la},{ns},{lo},{ew},1,12,0.62,{alt:.1f},M,{sep:.1f},M,,"),
        ck("GNGSA,A,3,05,13,15,18,23,29,,,,,,,1.10,0.62,0.91,1"),
        ck("GNGSA,A,3,72,81,82,,,,,,,,,,1.10,0.62,0.91,2"),
        ck("GNGSA,A,3,04,11,,,,,,,,,,,1.10,0.62,0.91,3"),
        ck("GPGSV,3,1,09,05,45,120,40,13,60,200,44,15,30,300,38,18,22,045,33,1"),
        ck("GPGSV,3,2,09,23,55,310,45,29,12,170,28,20,05,050,,193,70,180,42,1"),
        ck("GPGSV,3,3,09,42,48,160,35,1"),
        ck("GLGSV,1,1,04,72,20,100,30,81,70,010,42,82,35,270,39,88,03,330,,1"),
        ck("GAGSV,1,1,03,04,50,060,41,11,25,220,36,19,08,150,,7"),
        ck(f"GNGST,{hms},10,1.2,0.8,45,0.9,0.8,1.5"),
        ck(f"GNZDA,{hms},{tm.tm_mday:02d},{tm.tm_mon:02d},{tm.tm_year},00,00"),
    ]
    if n % 7 == 3:
        out.append("$GPGGA,BROKEN,1*00")         # bad checksum
    if n % 5 == 0:
        out.append("$PUBX,04,nochecksum")        # proprietary, no checksum
        out.append(ck("GNTXT,01,01,02,ANTSTATUS=OK"))
    return out

def serve(port, handler):
    srv = socket.create_server(("0.0.0.0", port))
    while True:
        c, _ = srv.accept()
        threading.Thread(target=handler, args=(c,), daemon=True).start()

def tick_loop(send):
    n = 0
    while True:
        now = time.time()
        time.sleep(1 - (now % 1) + 0.08)  # emit ~80ms after the second
        t = time.time()
        n += 1
        if not send(int(t), n, t):
            return

def raw_handler(c):
    def send(sec, n, t):
        try:
            c.sendall(("\r\n".join(epoch(sec, n)) + "\r\n").encode())
            return True
        except OSError:
            return False
    tick_loop(send)

def gpsd_handler(c):
    c.sendall(b'{"class":"VERSION","release":"3.25","rev":"3.25","proto_major":3,"proto_minor":15}\r\n')
    c.settimeout(10)
    buf = b""
    while b"?WATCH" not in buf:
        d = c.recv(1024)
        if not d:
            return
        buf += d
    sys.stderr.write("gpsd mock got: %r\n" % buf)
    dev = {"class": "DEVICE", "path": "/dev/ttyAMA0", "driver": "NMEA0183", "activated": "x", "bps": 9600}
    c.sendall((json.dumps({"class": "DEVICES", "devices": [dev]}) + "\r\n").encode())
    c.sendall(b'{"class":"WATCH","enable":true,"json":true,"raw":1,"pps":true}\r\n')
    def send(sec, n, t):
        lines = list(epoch(sec, n))
        ts = time.strftime("%Y-%m-%dT%H:%M:%S.000Z", time.gmtime(sec))
        lines.append(json.dumps({"class": "TPV", "device": "/dev/ttyAMA0", "mode": 3, "time": ts, "ept": 0.005}))
        lines.append(json.dumps({"class": "TOFF", "device": "/dev/ttyAMA0", "real_sec": sec, "real_nsec": 0,
                                 "clock_sec": sec, "clock_nsec": 81234567, "precision": -1}))
        lines.append(json.dumps({"class": "PPS", "device": "/dev/ttyAMA0", "real_sec": sec, "real_nsec": 0,
                                 "clock_sec": sec - 1, "clock_nsec": 999998765, "precision": -20}))
        try:
            c.sendall(("\r\n".join(lines) + "\r\n").encode())
            return True
        except OSError:
            return False
    tick_loop(send)

if __name__ == "__main__":
    ap = argparse.ArgumentParser(description="mock NMEA source for gnomon")
    ap.add_argument("--lat", type=float, default=POS["lat"], help="latitude, decimal degrees")
    ap.add_argument("--lon", type=float, default=POS["lon"], help="longitude, decimal degrees")
    ap.add_argument("--alt", type=float, default=POS["alt"], help="altitude in metres")
    ap.add_argument("mode", choices=["raw", "gpsd"])
    ap.add_argument("port", type=int)
    a = ap.parse_args()
    try:
        set_position(a.lat, a.lon, a.alt)
    except ValueError as e:
        ap.error(str(e))
    mode, port = a.mode, a.port
    serve(port, raw_handler if mode == "raw" else gpsd_handler)
