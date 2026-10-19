#!/usr/bin/env python3
"""Single-track proof: write one HD track of our own data as raw flux and
read it back with the controller's normal sector reads.

THIS OVERWRITES THE CHOSEN TRACK (side 0) OF THE DISK IN DRIVE 0.

usage: rawtrack-test.py [--track N] [--rate 40] [--overhead 3] [--build-only]
"""
import argparse, os, re, subprocess, sys, tempfile, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import rawtrack  # noqa: E402

ap = argparse.ArgumentParser()
ap.add_argument("--track", type=int, default=40)
ap.add_argument("--rate", type=int, default=40)
ap.add_argument("--sectors", type=int, default=21)
ap.add_argument("--overhead", type=int, default=3, help="DMA cycles added per flux byte")
ap.add_argument("--m65rs", default="/Applications/mega65studio/m65rs")
ap.add_argument("--acme", default="acme")
ap.add_argument("--build-only", action="store_true")
args = ap.parse_args()
here = os.path.dirname(os.path.abspath(__file__))
work = tempfile.mkdtemp(prefix="rawtrack-")

# Track contents: every sector different, so a misplaced sector shows.
data = [bytes((n * 29 + i * 7 + (i >> 8)) & 0xFF for i in range(512)) for n in range(1, args.sectors + 1)]
marks = []
cells = rawtrack.track_cells(args.track, data, args.rate, gap_marks=marks)
fx = rawtrack.flux(cells, args.rate, args.overhead)
split = rawtrack.split_point(cells, marks)
cycles = sum(2 * v + args.overhead for v in fx)
print(f"track {args.track}: {args.sectors} sectors, {len(fx)} flux bytes, split at {split}, "
      f"{100 * cycles / rawtrack.revolution_cycles():.1f}% of a revolution")
if cycles > rawtrack.revolution_cycles() * 0.985:
    sys.exit("Track too long for one revolution; use fewer sectors.")

# Routine with the job lengths patched in.
sym = os.path.join(work, "rawwrite.sym")
binf = os.path.join(work, "rawwrite.bin")
subprocess.run([args.acme, "-f", "plain", "-o", binf, "-l", sym, os.path.join(here, "rawwrite.a")],
               check=True, capture_output=True)
syms = {m[0]: int(m[1], 16) for m in re.findall(r"(\w+)\s*=\s*\$(\w+)", open(sym).read())}
code = bytearray(open(binf, "rb").read())


def put(label, value, size=2):
    o = syms[label] - 0x1600
    code[o:o + size] = value.to_bytes(size, "little")


src2 = 0x40000 + split
put("len1", split)
put("len2", len(fx) - split)
put("src2", src2 & 0xFFFF)
code[syms["src2"] - 0x1600 + 2] = src2 >> 16
open(binf, "wb").write(code)
fluxf = os.path.join(work, "flux.bin")
open(fluxf, "wb").write(fx)
if args.build_only:
    print("built", binf, fluxf)
    sys.exit(0)


def m65(*a):
    out = subprocess.run([args.m65rs, *a], capture_output=True, text=True, timeout=120)
    if out.returncode:
        sys.exit(f"m65rs {' '.join(a)} failed: {out.stderr.strip()}")
    return out.stdout


def peek(addr):
    return int(m65("peek", f"{addr:x}").split("$")[-1], 16)


def poke(addr, *vals):
    m65("poke", f"{addr:x}", *[f"{v:02x}" for v in vals])


IO = 0xFFD3000


def wait_idle(timeout=3.0):
    end = time.time() + timeout
    while peek(IO + 0x082) & 0x80:
        if time.time() > end:
            return False
    return True


m65("load", f"{fluxf}@40000")
m65("load", f"{binf}@1600")
poke(IO + 0x6A1, 0x01)
poke(IO + 0x696, 0x00)
poke(IO + 0x689, peek(IO + 0x689) & 0x7F)
poke(IO + 0x080, 0x68)
poke(IO + 0x6AE, 0x00)
poke(IO + 0x6A2, args.rate)
time.sleep(1.0)
st = peek(IO + 0x082)
if st & 0x02:
    sys.exit("Disk is write-protected.")
for _ in range(90):
    if peek(IO + 0x082) & 1:
        break
    poke(IO + 0x081, 0x10)
    wait_idle()
for _ in range(args.track):
    poke(IO + 0x081, 0x18)
    wait_idle()
poke(IO + 0x084, args.track)
poke(IO + 0x085, args.sectors)
poke(IO + 0x086, 0)

m65("go", "1600")
end = time.time() + 6
while time.time() < end and peek(syms["done"]) != 1:
    time.sleep(0.2)
print("write routine done:", peek(syms["done"]) == 1)

# Read back, following the Track Info Block.
poke(IO + 0x6A2, 40)
poke(IO + 0x6AE, 0xE0)
end = time.time() + 3
while time.time() < end:
    if (peek(IO + 0x6A6), peek(IO + 0x6A7)) == (args.track, args.rate):
        break
tib = [peek(IO + 0x6A6 + i) for i in range(4)]
print("TIB seen:", tib)
good, wrong, missing, crc = 0, [], [], []
for n in range(1, args.sectors + 1):
    poke(IO + 0x081, 0x00)
    poke(IO + 0x084, args.track)
    poke(IO + 0x085, n)
    poke(IO + 0x086, 0)
    poke(IO + 0x081, 0x01)
    poke(IO + 0x081, 0x40)
    time.sleep(0.01)
    done = wait_idle(2.5)
    st = peek(IO + 0x082)
    if not done or st & 0x10:
        missing.append(n)
        continue
    if st & 0x08:
        crc.append(n)
        continue
    back = bytes.fromhex("".join(
        line.split(":")[1].split("  ")[0].replace(" ", "")
        for line in m65("dump", "ffd6c00", "512").splitlines() if ":" in line))
    if back == data[n - 1]:
        good += 1
    else:
        wrong.append(n)
print(f"good {good}/{args.sectors}, wrong data {wrong or '-'}, CRC {crc or '-'}, not found {missing or '-'}")
poke(IO + 0x080, 0x00)
