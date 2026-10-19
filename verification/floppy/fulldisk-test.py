#!/usr/bin/env python3
"""Full-disk raw-flux test: write every track and side of the disk in drive 0
at the standard HD rate (21 x 512-byte sectors per track side) with unique
data, and verify each track side straight after writing it.

THIS ERASES THE WHOLE DISK IN DRIVE 0.

usage: fulldisk-test.py [--tracks 83] [--compare-every 10]
"""
import argparse, os, re, subprocess, sys, tempfile, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import rawtrack  # noqa: E402

ap = argparse.ArgumentParser()
ap.add_argument("--tracks", type=int, default=83)
ap.add_argument("--first", type=int, default=0)
ap.add_argument("--rate", type=int, default=40)
ap.add_argument("--sectors", type=int, default=21)
ap.add_argument("--overhead", type=int, default=3)
ap.add_argument("--lead-in", type=int, default=40, help="$00 bytes before the TIB (core format: 14)")
ap.add_argument("--tib-gap", type=int, default=24, help="$4E bytes between TIB and sector 1 (core: 2)")
ap.add_argument("--compare-every", type=int, default=10,
                help="compare sector bytes on every Nth track (others: controller CRC check only)")
ap.add_argument("--m65rs", default="/Applications/mega65studio/m65rs")
ap.add_argument("--acme", default="acme")
args = ap.parse_args()
here = os.path.dirname(os.path.abspath(__file__))
work = tempfile.mkdtemp(prefix="fulldisk-")
IO = 0xFFD3000


def m65(*a):
    out = subprocess.run([args.m65rs, *a], capture_output=True, text=True, timeout=120)
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


def sector_data(track, side, n):
    seed = (track * 2 + side) * 32 + n
    return bytes((seed * 29 + i * 7 + (i >> 8) + track) & 0xFF for i in range(512))


sym = os.path.join(work, "rawwrite.sym")
binf = os.path.join(work, "rawwrite.bin")
subprocess.run([args.acme, "-f", "plain", "-o", binf, "-l", sym, os.path.join(here, "rawwrite.a")],
               check=True, capture_output=True)
syms = {m[0]: int(m[1], 16) for m in re.findall(r"(\w+)\s*=\s*\$(\w+)", open(sym).read())}
m65("load", f"{binf}@1600")

poke(IO + 0x6A1, 0x01)
poke(IO + 0x696, 0x00)
poke(IO + 0x689, peek(IO + 0x689) & 0x7F)
poke(IO + 0x080, 0x68)
poke(IO + 0x6AE, 0x00)
time.sleep(1.0)
if peek(IO + 0x082) & 0x02:
    sys.exit("Disk is write-protected.")
for _ in range(90):
    if peek(IO + 0x082) & 1:
        break
    poke(IO + 0x081, 0x10)
    wait_idle()
for _ in range(args.first):
    poke(IO + 0x081, 0x18)
    wait_idle()

start = time.time()
totals = dict(good=0, crc=0, missing=0, wrong=0, sides=0)
log = []
for track in range(args.first, args.first + args.tracks):
    if track > args.first:
        poke(IO + 0x081, 0x18)
        wait_idle()
    for side in (0, 1):
        data = [sector_data(track, side, n) for n in range(1, args.sectors + 1)]
        marks = []
        cells = rawtrack.track_cells(track, data, args.rate, side=side, gap_marks=marks,
                                      lead_in=args.lead_in, tib_gap=args.tib_gap)
        fx = rawtrack.flux(cells, args.rate, args.overhead)
        if sum(2 * v + args.overhead for v in fx) > rawtrack.revolution_cycles() * 0.985:
            sys.exit("Track too long for one revolution.")
        split = rawtrack.split_point(cells, marks)
        fluxf = os.path.join(work, "flux.bin")
        open(fluxf, "wb").write(fx)
        m65("load", f"{fluxf}@40000")
        src2 = 0x40000 + split
        poke(syms["len1"], split & 0xFF, split >> 8)
        rest = len(fx) - split
        poke(syms["len2"], rest & 0xFF, rest >> 8)
        poke(syms["src2"], src2 & 0xFF, src2 >> 8 & 0xFF, src2 >> 16)
        poke(IO + 0x080, 0x68 if side == 0 else 0x60)
        poke(IO + 0x084, track)
        poke(IO + 0x086, side)
        poke(IO + 0x6AE, 0x00)
        poke(syms["done"], 0)
        m65("go", "1600")
        end = time.time() + 6
        while time.time() < end and peek(syms["done"]) != 1:
            time.sleep(0.1)
        if peek(syms["done"]) != 1:
            sys.exit(f"Write routine did not finish on track {track} side {side}.")
        # Verify: follow the TIB, read every sector.
        poke(IO + 0x6A2, 40)
        poke(IO + 0x6AE, 0xE0)
        end = time.time() + 2
        while time.time() < end and (peek(IO + 0x6A6), peek(IO + 0x6A9)) != (track, args.sectors):
            pass
        good, crc, missing, wrong = 0, [], [], []
        compare = track % args.compare_every == 0
        for n in range(1, args.sectors + 1):
            poke(IO + 0x081, 0x00)
            poke(IO + 0x084, track)
            poke(IO + 0x085, n)
            poke(IO + 0x086, side)
            poke(IO + 0x081, 0x01)
            poke(IO + 0x081, 0x40)
            done = wait_idle(2.5)
            st = peek(IO + 0x082)
            if not done or st & 0x10:
                missing.append(n)
            elif st & 0x08:
                crc.append(n)
            elif compare:
                back = bytes.fromhex("".join(
                    line.split(":")[1].split("  ")[0].replace(" ", "")
                    for line in m65("dump", "ffd6c00", "512").splitlines() if ":" in line))
                if back == data[n - 1]:
                    good += 1
                else:
                    wrong.append(n)
            else:
                good += 1
        totals["good"] += good
        totals["crc"] += len(crc)
        totals["missing"] += len(missing)
        totals["wrong"] += len(wrong)
        totals["sides"] += 1
        line = (f"T{track:02d}/S{side}: {good}/{args.sectors} ok"
                f"{' (bytes compared)' if compare else ''}"
                f"{f', CRC {crc}' if crc else ''}{f', missing {missing}' if missing else ''}"
                f"{f', wrong {wrong}' if wrong else ''}")
        print(line, flush=True)
        log.append(line)

poke(IO + 0x080, 0x00)
mins = (time.time() - start) / 60
print(f"\n{totals['sides']} track sides in {mins:.1f} min: {totals['good']} good sectors "
      f"of {totals['sides'] * args.sectors} ({totals['good'] * 512 / 1e6:.2f} MB), "
      f"CRC {totals['crc']}, missing {totals['missing']}, wrong {totals['wrong']}")
