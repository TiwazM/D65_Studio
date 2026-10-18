#!/usr/bin/env python3
"""One-track floppy capacity test for the MEGA65, driven over the JTAG serial
monitor with m65rs (no program runs on the MEGA65).

For each (encoding, data rate) it formats ONE track of the disk in the
internal drive with the core's hardware auto-format ($D081 = $A0), which
writes a Track Information Block and as many 512-byte sectors as fit, each
filled with the known pattern (62 + i) & $FF. It then reads every sector
back through the floppy controller and counts the sectors that come back
with the right data.

THIS ERASES THE CHOSEN TRACK OF THE DISK IN DRIVE 0. Use a spare HD disk.

usage: track-capacity.py [--track N] [--m65rs PATH] [--dry-run]
"""
import argparse, subprocess, sys, time

ap = argparse.ArgumentParser()
ap.add_argument("--track", type=int, default=75)
ap.add_argument("--m65rs", default="/Applications/mega65studio/m65rs")
ap.add_argument("--dry-run", action="store_true", help="only read status registers")
ap.add_argument("--settings", default="MFM:40,MFM:32,MFM:29,RLL:27,RLL:25,RLL:23,RLL:21")
args = ap.parse_args()


def m65(*a):
    out = subprocess.run([args.m65rs, *a], capture_output=True, text=True, timeout=60)
    if out.returncode:
        sys.exit(f"m65rs {' '.join(a)} failed: {out.stderr.strip()}")
    return out.stdout


def peek(reg):
    return int(m65("peek", f"ffd3{reg:03x}").split("$")[-1], 16)


def poke(reg, *vals):
    m65("poke", f"ffd3{reg:03x}", *[f"{v:02x}" for v in vals])


def wait_idle(timeout=3.0):
    end = time.time() + timeout
    while peek(0x082) & 0x80:
        if time.time() > end:
            return False
    return True


def status():
    a, b = peek(0x082), peek(0x083)
    return dict(busy=a >> 7 & 1, rnf=a >> 4 & 1, crc=a >> 3 & 1, prot=a >> 1 & 1,
                tk0=a & 1, diskin=b >> 3 & 1, index=b >> 2 & 1)


print("drive 0 real:", peek(0x6A1) & 1, " status:", status(),
      " rate $D6A2:", peek(0x6A2), " enc $D6AE: $%02X" % peek(0x6AE))
if args.dry_run:
    sys.exit(0)

poke(0x6A1, 0x01)          # use the real drive for drive 0
poke(0x696, 0x00)          # no auto track seeking
poke(0x080, 0x68)          # motor on, side 0
time.sleep(1.0)            # spin up
st = status()
if not st["diskin"]:
    sys.exit("No disk in the drive.")
if st["prot"]:
    sys.exit("The disk is write-protected; slide the tab to allow writing.")

# Head to track 0, then step in to the test track.
for _ in range(90):
    if peek(0x082) & 1:
        break
    poke(0x081, 0x10)
    wait_idle()
else:
    sys.exit("Track 0 not found.")
for _ in range(args.track):
    poke(0x081, 0x18)
    wait_idle()

expected = bytes((62 + i) & 0xFF for i in range(512))
results = []
for setting in args.settings.split(","):
    enc, rate = setting.split(":")
    rate = int(rate)
    # Format: fixed rate and encoding, TIB says up to 64 sectors.
    poke(0x6AE, 0x01 if enc == "RLL" else 0x00)
    poke(0x6A2, rate)
    poke(0x084, args.track)
    poke(0x085, 64)
    poke(0x086, 0)
    poke(0x081, 0x00)
    wait_idle()
    poke(0x081, 0xA0)                     # hardware format, with gaps, no precomp
    if not wait_idle(timeout=5.0):
        sys.exit("Format did not finish.")
    # Read back at the same rate and encoding, set explicitly: letting the
    # Track Info Block choose ($D6AE = $F0) used a stale TIB from an earlier
    # format and found nothing.
    poke(0x6AE, 0x40 | (0x01 if enc == "RLL" else 0x00))
    poke(0x6A2, rate)
    time.sleep(0.4)
    tib = dict(track=peek(0x6A6), rate=peek(0x6A7), enc=peek(0x6A8), sectors=peek(0x6A9))
    good, bad = 0, []
    for s in range(1, 65):
        poke(0x081, 0x00)
        poke(0x084, args.track)
        poke(0x085, s)
        poke(0x086, 0)
        poke(0x081, 0x01)
        poke(0x081, 0x40)
        time.sleep(0.01)
        ok_cmd = wait_idle(timeout=2.0)
        st = status()
        if not ok_cmd or st["rnf"]:
            break                           # first missing sector ends the track
        if st["crc"]:
            bad.append(s)
            continue
        data = bytes.fromhex("".join(
            line.split(":")[1].split("  ")[0].replace(" ", "")
            for line in m65("dump", "ffd6c00", "512").splitlines() if ":" in line))
        if data == expected:
            good += 1
        else:
            bad.append(s)
    last = s - 1
    bpt = good * 512
    results.append((enc, rate, tib, last, good, bad))
    print(f"{enc} rate {rate:2d}: TIB {tib} | sectors found {last}, good {good}, "
          f"bad {bad or '-'} | {bpt} bytes/track side | x83x2 = {bpt * 166 / 1e6:.2f} MB")

poke(0x080, 0x00)  # motor off
print("\nsummary")
for enc, rate, tib, last, good, bad in results:
    print(f"  {enc} {rate:2d}: {good} good sectors on track {args.track}")
