#!/usr/bin/env python3
"""Write one track side as raw flux with its Track Info Block at one rate and
its sectors at another (e.g. a DD-rate TIB in front of ~60-rate data), then
read the sectors back at the data rate.

THIS OVERWRITES THAT TRACK SIDE OF THE DISK IN DRIVE 0.

usage: mixedtrack.py CYLINDER SIDE DATA_RATE TIB_RATE SECTORS
"""
import os, re, subprocess, sys, tempfile, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import rawtrack  # noqa: E402

M65RS = "/Applications/mega65studio/m65rs"
IO = 0xFFD3000
LEAD_IN, TIB_GAP = 40, 24
cyl, side, data_rate, tib_rate, count = (int(a) for a in sys.argv[1:6])
here = os.path.dirname(os.path.abspath(__file__))
work = tempfile.mkdtemp(prefix="mixed-")


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


data = [bytes((cyl * 31 + side * 7 + n * 17 + i * 5 + (i >> 8)) & 0xFF for i in range(512))
        for n in range(1, count + 1)]
marks = []
cells = rawtrack.track_cells(cyl, data, data_rate, side=side, gap_marks=marks,
                             lead_in=LEAD_IN, tib_gap=TIB_GAP)
# TIB block ends after lead-in, 3 sync marks, 7 TIB bytes, 4 x $00 and the gap.
boundary = (LEAD_IN + 3 + 7 + 4 + TIB_GAP) * 16
while not cells[boundary - 1]:
    boundary -= 1
fx = rawtrack.flux(cells[:boundary], tib_rate, 3) + rawtrack.flux(cells[boundary:], data_rate, 3)
cycles = sum(2 * v + 3 for v in fx)
print(f"{count} sectors at rate {data_rate}, TIB at rate {tib_rate}: {len(fx)} flux bytes, "
      f"{100 * cycles / rawtrack.revolution_cycles():.1f}% of a revolution")
if cycles > rawtrack.revolution_cycles() * 0.985:
    sys.exit("Track too long for one revolution.")
split = rawtrack.split_point(cells, marks) or len(fx)
sym = os.path.join(work, "rawwrite.sym")
binf = os.path.join(work, "rawwrite.bin")
subprocess.run(["acme", "-f", "plain", "-o", binf, "-l", sym, os.path.join(here, "rawwrite.a")],
               check=True, capture_output=True)
syms = {m[0]: int(m[1], 16) for m in re.findall(r"(\w+)\s*=\s*\$(\w+)", open(sym).read())}
fluxf = os.path.join(work, "flux.bin")
open(fluxf, "wb").write(fx)
m65("stop")
m65("load", f"{binf}@1600")
m65("load", f"{fluxf}@40000")
poke(syms["len1"], split & 0xFF, split >> 8)
rest = len(fx) - split
src2 = 0x40000 + split
poke(syms["len2"], rest & 0xFF, rest >> 8)
poke(syms["src2"], src2 & 0xFF, src2 >> 8 & 0xFF, src2 >> 16)
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
poke(syms["done"], 0)
m65("go", "1600")
time.sleep(0.6)
end = time.time() + 6
while time.time() < end and peek(syms["done"]) != 1:
    time.sleep(0.1)
if peek(syms["done"]) != 1:
    sys.exit("Write routine did not finish.")
# Fresh TIB: a step clears the registers.
poke(IO + 0x081, 0x18)
wait_idle()
poke(IO + 0x081, 0x10)
wait_idle()
poke(IO + 0x6A2, tib_rate)
poke(IO + 0x6AE, 0xE0)
end = time.time() + 3
while time.time() < end and peek(IO + 0x6A6) != cyl:
    pass
print("TIB:", [peek(IO + 0x6A6 + i) for i in range(4)])
poke(IO + 0x6AE, 0x00)
poke(IO + 0x6A2, data_rate)
good = 0
for n in range(1, count + 1):
    poke(IO + 0x081, 0x00)
    poke(IO + 0x084, cyl)
    poke(IO + 0x085, n)
    poke(IO + 0x086, side)
    poke(IO + 0x081, 0x01)
    poke(IO + 0x081, 0x40)
    ok = wait_idle(2.5) and not peek(IO + 0x082) & 0x18
    good += ok
print(f"sectors read back at rate {data_rate}: {good}/{count}")
poke(IO + 0x080, 0x00)
