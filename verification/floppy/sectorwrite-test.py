#!/usr/bin/env python3
"""Sector write test ($D081=$80) on the real drive at a given rate, on a disk
whose tracks carry Track Info Blocks (a DiskLab HD floppy).

Writes one 512-byte sector of unique data, reads it back, and checks that the
neighbouring sectors still read. THIS OVERWRITES THAT SECTOR.

usage: sectorwrite-test.py CYLINDER SIDE SECTOR RATE
"""
import os, subprocess, sys, tempfile, time

M65RS = "/Applications/mega65studio/m65rs"
IO = 0xFFD3000
cyl, side, sector, rate = (int(a) for a in sys.argv[1:5])


def m65(*a):
    out = subprocess.run([M65RS, *a], capture_output=True, text=True, timeout=120)
    if out.returncode:
        sys.exit(f"m65rs {' '.join(a)} failed: {out.stderr.strip()}")
    return out.stdout


def peek(addr):
    return int(m65("peek", f"{addr:x}").split("$")[-1], 16)


def poke(addr, *vals):
    m65("poke", f"{addr:x}", *[f"{v:02x}" for v in vals])


def wait_idle(timeout=3.0):
    end = time.time() + timeout
    while peek(IO + 0x082) & 0x80:
        if time.time() > end:
            return False
    return True


def buffer():
    return bytes.fromhex("".join(
        line.split(":")[1].split("  ")[0].replace(" ", "")
        for line in m65("dump", "ffd6c00", "512").splitlines() if ":" in line))


def read(n):
    poke(IO + 0x081, 0x00)
    poke(IO + 0x084, cyl)
    poke(IO + 0x085, n)
    poke(IO + 0x086, side)
    poke(IO + 0x081, 0x01)
    poke(IO + 0x081, 0x40)
    done = wait_idle(2.5)
    st = peek(IO + 0x082)
    return done and not st & 0x18, st


poke(IO + 0x6A1, 0x01)
poke(IO + 0x696, 0x00)
poke(IO + 0x689, peek(IO + 0x689) & 0x7F)
poke(IO + 0x080, 0x68 if side == 0 else 0x60)
poke(IO + 0x6AE, 0x00)
time.sleep(1.0)
if peek(IO + 0x082) & 0x02:
    sys.exit("Disk is write-protected.")
for _ in range(90):
    if peek(IO + 0x082) & 1:
        break
    poke(IO + 0x081, 0x10)
    wait_idle()
for _ in range(cyl):
    poke(IO + 0x081, 0x18)
    wait_idle()
poke(IO + 0x6A2, rate)
poke(IO + 0x6AE, 0xE0)          # follow the TIB, HD decoder on
end = time.time() + 3
while time.time() < end and peek(IO + 0x6A6) != cyl:
    pass
print(f"TIB: track {peek(IO + 0x6A6)} rate {peek(IO + 0x6A7)} sectors {peek(IO + 0x6A9)}")

ok, st = read(sector)
print(f"before: sector {sector} read {'ok' if ok else 'FAILED'} (status ${st:02x})")
data = bytes((cyl * 37 + side * 11 + sector * 5 + i * 13 + (i >> 8)) & 0xFF for i in range(512))
f = os.path.join(tempfile.mkdtemp(), "sector.bin")
open(f, "wb").write(data)
m65("load", f"{f}@ffd6c00")
poke(IO + 0x081, 0x00)
poke(IO + 0x084, cyl)
poke(IO + 0x085, sector)
poke(IO + 0x086, side)
poke(IO + 0x081, 0x80)
done = wait_idle(3.0)
st = peek(IO + 0x082)
print(f"write: {'finished' if done else 'still busy'}, status ${st:02x}"
      f"{' RNF' if st & 0x10 else ''}{' CRC' if st & 0x08 else ''}")
m65("fill", "ffd6c00", "ffd6e00", "00")
ok, st = read(sector)
back = buffer() if ok else b""
print(f"after: sector {sector} read {'ok' if ok else 'FAILED'} (status ${st:02x}), "
      f"data {'MATCHES' if back == data else 'differs'}")
for n in (sector - 1, sector + 1):
    if n >= 1:
        ok, st = read(n)
        print(f"neighbour sector {n}: {'ok' if ok else 'FAILED'} (status ${st:02x})")
poke(IO + 0x080, 0x00)
