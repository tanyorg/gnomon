#!/usr/bin/env python3
# Emulate a GNSS receiver on a pseudo-terminal for testing serial input.
#   mockserial.py [--baud 9600] [--offset 0.080] [--garbage] [--die-after N]
# Prints the slave tty path on stdout, then streams NMEA paced at the given
# line rate (10 bits per char), starting OFFSET seconds after each second.
import argparse, os, pty, sys, time, tty

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from mockgps import epoch  # noqa: E402

ap = argparse.ArgumentParser()
ap.add_argument("--baud", type=int, default=9600)
ap.add_argument("--offset", type=float, default=0.080)
ap.add_argument("--garbage", action="store_true", help="send non-NMEA noise only")
ap.add_argument("--die-after", type=int, default=0, help="close the pty after N epochs")
a = ap.parse_args()

master, slave = pty.openpty()
tty.setraw(slave)
print(os.ttyname(slave), flush=True)
cps = a.baud / 10.0
n = 0
while True:
    now = time.time()
    time.sleep(1 - (now % 1) + a.offset)
    sec = int(time.time())
    n += 1
    if a.garbage:
        data = bytes((i * 37 + n) % 251 for i in range(600))
    else:
        data = ("\r\n".join(epoch(sec, n)) + "\r\n").encode()
    # pace in 16-byte slices to approximate the wire rate
    t0 = time.time()
    for i in range(0, len(data), 16):
        os.write(master, data[i:i + 16])
        due = t0 + (i + 16) / cps
        d = due - time.time()
        if d > 0:
            time.sleep(d)
    if a.die_after and n >= a.die_after:
        os.close(master)
        time.sleep(30)
        break
