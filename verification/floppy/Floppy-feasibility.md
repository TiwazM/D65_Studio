# Writing DiskLab images to a real MEGA65 floppy: what the core supports

Read on 19 October 2026 from the MEGA65 core (`mega65-core`, development branch),
`mega65-tools` and the MEGA65 Book. Line numbers refer to
`src/vhdl/sdcardio.vhdl` unless another file is named. Nothing here was run on
hardware yet.

## What the team designed

- **Formatting exists in hardware.** `$D081` = `$A0`/`$A4`/`$A8`/`$AC` runs an
  automatic track format (2478–2509). `$A4` adds write precompensation, `$A8` is
  "Amiga-style track-at-once". `$A1`/`$A5` is the C65-style unbuffered format,
  where the CPU feeds bytes through `$D087`/`$D088` (3669–3725). Formatting only
  works on a real drive; with an image mounted the core logs "Ignoring track
  format, due to using D81 image" (2512).
- **Denser encodings exist and can be written.** `$D6AE` bits 0–3 select
  0 = MFM, 1 = RLL2,7, F = raw (1764–1793). `rll27_bits_to_gaps.vhdl`
  implements RLL writing, including write precompensation.
- **The data rate is variable.** `$D6A2` is "bus cycles per magnetic interval" at
  40.5 MHz: DD = 81 (`$51`), HD = 40 (`$28`). mega65-tools' `floppytest.c` uses
  MFM rates 29–32 and RLL rates 23–26 in four track zones.
- **Track Information Blocks.** The automatic format writes a TIB after the
  index pulse (3944–4014): 3×`$A1`, `$65`, track, rate, encoding, sector count
  and a CRC. On read, the TIB sets the rate and encoding automatically
  (`$D6A6`–`$D6A9`, `$D6AE` bits 5 and 7; 1832–1864). This is the
  "MEGA65 HD floppy" format.
- **Raw flux writing.** DMA option `$0D` writes flux intervals
  (`gs4510.vhdl` 5894–5901). No tool uses it yet.
- **Capacity the team claims.** The docs say "HD+ disk formats that are able to
  fit around 3MiB" and "likely in the range of 2MB to 3.5MB". `floppytest.c`
  formats 83 tracks × 2 sides with 37/36/35/34 RLL sectors per zone, about
  3.0 MB.

## Gaps to be aware of

- **Sector writes on HD/RLL tracks probably don't work yet.** The sector write
  path (4023–4140) waits for `fdc_sector_found` from the decoder that is
  hard-wired to DD MFM (916–944). The HD/RLL decoder's signal is only checked on
  the read path (4152, 4253). In practice, data at HD/RLL rates is written by
  formatting (track-at-once) or as raw flux, not by sector writes. This was read
  from the code, not tested.
- **The ROM and Hyppo can't use HD floppies.** Hyppo only mounts images and
  switches the real drive on or off (`dos.asm` 4295–4441). The book says the
  drive "expects a Double Density (DD) floppy disk" and that full-capacity HD
  support may come "in a future firmware update". So a program on an HD disk
  can't be LOADed by the ROM; it needs its own reader.
- **D65 is not the image of a real floppy.** The core comment says "MEGA65 HD
  disks support 85 tracks and 64 sectors per track" (2228), but 64 × 512-byte
  sectors per track side would need about 2.5 times the fastest rate anyone has
  used. The F011 offset for D65 images is `track×128 + physical_sector`, where
  side 1 adds only 9 (`sector+9`, left over from the 1581 layout, 2168–2172).
  With the `track ≥ 84 or sector > 63` clamp, the F011 path reaches about half
  the image, and the two sides overlap. That's why DiskLab's reader reads the SD
  card directly instead.

## Capacity per track side (computed; tool figures in brackets)

At 300 rpm one revolution is 8.1 M cycles. Both encodings use 16 cells per
byte. A sector with gaps takes 587 bytes on the track, without gaps 529.

| Encoding @ `$D6A2` | Bytes per track side | 512-byte sectors, with / without gaps | 85 tracks × 2 sides, with gaps |
|---|---|---|---|
| MFM DD @ 81 (1581) | 6,250 | 10 / 11 | 870 KB |
| MFM HD @ 40 | 12,656 | 21 / 23 [21 / 24] | 1.83 MB |
| MFM HD+ @ 29 | 17,457 | 29 / 32 [29; 32] | 2.52 MB |
| RLL2,7 @ 27 (HD flux spacing) | 18,750 | 31 / 35 [32 / 36] | 2.70 MB |
| RLL2,7 @ 23 (fastest zone in use) | 22,011 | 37 / 41 [37 / 42] | 3.22 MB |
| floppytest RLL zones, 83 × 2 | — | 37–34 | about 3.0 MB |
| A full D65 | — | 64 | 5.57 MB (does not fit) |

So a real HD disk can realistically hold about **2.5–3 MB**: about half a full
D65.

## What a DiskLab floppy writer would need

1. **Hardware:** the internal 3.5" drive (or an external HD drive) and HD disks.
   DD disks can't hold the higher rates.
2. **A layout:** a "DiskLab HD" format of 83 tracks × 2 sides in RLL2,7 zones,
   with a TIB per track. The image would be the same files in a smaller
   container, with START and the directory kept on track 40 in plain 1581
   geometry so a disk directory still lists.
3. **A writer running on the MEGA65** (the PC can't drive the floppy directly):
   upload a small program over JTAG, stream the image in, and for each track set
   the rate and encoding, then use the automatic format (`$A0`/`$A8`) with the
   track's sectors pre-filled. Track-at-once writes data while formatting, which
   gets around the DD-only sector write. Then read each track back to verify it.
4. **A floppy version of the resident reader:** select the real drive and read
   sectors through the F011 path with TIB auto-rate instead of SD sectors. The
   reader is out of room today, so this would be a separate reader build.
5. **A cheap first test** (writes to a spare HD disk only): format one track at
   rates 40 → 23 in MFM and RLL, write a pattern, read it back, and record which
   sector counts survive on this drive and media.

## First hardware test, 19 October 2026 (MEGA65 R6, internal drive, spare HD disk)

`track-capacity.py`, track 75, side 0, hardware format `$D081 = $A0`:

- **MFM, rate 40 (standard HD):** format and read-back both work. The TIB reads
  back as written. 21 sectors come back byte-for-byte with the pattern; sector 22
  is cut off by the index hole. That's 10,752 bytes per track side, about
  **1.78 MB** over 83 tracks × 2 sides (a PC 1.44 MB disk uses 18 sectors).
- **MFM 32/29 and RLL2,7 27/25/23/21:** the format runs and the TIB reads back
  with the requested rate and encoding, so the track was written. At MFM 32 the
  HD decoder sees sector headers up to 26 per track side, but full sector reads
  fail. The headers carry side byte `$80` and track `$CB` (75 with bit 7 set),
  the side never matches, and reads end in RNF, a CRC error, or a controller
  that stays busy. At RLL 27 almost no headers decode. So beyond the standard HD
  rate, the read side does not work yet on this core and drive.
- Letting the TIB pick the read rate (`$D6AE = $F0`) used a stale TIB from an
  earlier format, so the script sets rate and encoding explicitly.
- Afterwards the MEGA65 was reset: drive 0 is back to normal (`$D6A1` = `$08`).

Raw log: `track-capacity-run2.log`.

## Second session, 19 October 2026

- The MEGA65 team's `floppycapacity` test (mega65-tools, built with cc65 2.18;
  the upstream file is missing its final `}`) hung on its first step. It erases
  a track with the C65-style unbuffered format (`$D081 = $A1`) and waits for
  DRQ (`$D082` bit 6), which never came on this core. Stopped after 8 minutes.
- `track-capacity.py --erase` (a standard-HD format first, then the test
  format, on track 75 of the same disk) got **no** clean sectors at any
  setting, including the standard HD rate that worked in the first run (11
  sectors found, all failing). The TIBs read back inconsistently. Log:
  `track-capacity-run3-erase.log`.
- So far only one run, on fresh media, gave clean HD sectors. Either the disk
  or drive is marginal at HD rates, or the core's HD read path isn't
  dependable yet. A new HD disk (and a check that this one really is HD) is the
  next thing to rule out before building anything on top.

## Third session, 19 October 2026: following the TIB (gardners' hint)

Reads now follow the Track Info Block: `$D6AE` bit 7 (TIBEN, encoding from the
TIB) and bit 5 (VARSPD, data rate from the TIB), plus bit 6 (HD decoder), and
the test waits until `$D6A6`-`$D6A8` show the TIB just written. Track 40.

- MFM 40: 20-21 clean sectors (TIB followed correctly).
- MFM 32 and 29, with `$A0` and with `$A8` (track-at-once): the new TIB is seen,
  headers for sectors 1-20+ decode (`$D6A3` shows track `$A8` = 40 with bit 7
  set, `$D6A5` side `$80`), but a sector read (`$D081 = $40`) never finishes:
  BUSY stays set, no RNF, no CRC, whatever track/side byte is requested
  (40 or `$A8`, 0 or `$80`). The script's earlier "not found" results at these
  rates were reads timing out.
- RLL 27/25/23: with `$A0` the TIB is not always seen; with `$A8` it is (encoding
  byte reads 1 instead of `$41`). Reads behave the same as MFM 32.

So the rate switch works and headers decode; the sector read state machine is
what doesn't complete above the standard HD rate. Log:
`track-capacity-run5-tib-follow.log`.

## Sector write test, 19 October 2026

Track 40, format at the given rate, then `$D081=$80` on sector 5 with a new
pattern, then read it back (`track-capacity-run6-write.log`):

- **HD (rate 40):** the write ends with RNF, and sector 5 still holds the
  format pattern. Nothing was written.
- **DD control (rate 81):** the write succeeds and reads back as written.

This matches `F011WriteSectorRealDriveWait`, which waits on `fdc_sector_found`
from the 81-cycle decoder only. Writing our own data at HD therefore needs
either a core change (also accept `fdc_sector_found_2x` there, as the read
path does) or writing whole tracks as raw flux by DMA (`$D705` option `$0D`).

## Raw-flux track write: works (19 October 2026)

`rawtrack.py` builds a track exactly like the core's own HD format (TIB, 21
sectors, same gaps and syncs) but with our data, as one DMA byte per flux
transition. `rawwrite.a` starts the C65-style unbuffered format
(`$D081=$A1`), which holds the write gate open from one index pulse to the
next and, being fed no bytes, writes nothing itself; the routine streams the
flux with DMA option `$0D` from chip RAM `$40000` (two chained jobs, split in a
gap). `rawtrack-test.py` runs it and reads the sectors back with the normal
`$D081=$40` reads, following the TIB.

- First run: the data was on the disk (decoded from a raw flux capture,
  `$D705` option `$0F`) and 20/21 sectors read back with the rate set by hand,
  but the TIB and sector 1 were lost: the DMA was started at the beginning of
  the index pulse, while the controller opens the gate at its end
  (`f_index_rising_edge` = index line going inactive = `$D083` bit 2 falling).
- After waiting for the end of the pulse: **21/21 sectors byte-perfect, and the
  controller follows our own TIB** (track 40, rate 40, 21 sectors), twice in a
  row. 96.7% of a revolution with 3 cycles of DMA overhead allowed per flux
  byte. Measured flux: 2/3/4-cell intervals read back as about 80/114/152
  cycles (normal MFM peak shift; no precompensation applied).

So a standard-HD floppy writer (~1.78 MB) is possible on this core without any
core change: write tracks as raw flux, read with the normal sector reads.
Log: `rawtrack-runs.log`.

## Full disk, 19 October 2026: 1.78 MB, no errors

`fulldisk-test.py`: all 83 tracks x 2 sides of a Sony 2HD disk written as raw
flux at the standard HD rate (21 x 512 bytes per track side, unique data in
every sector, 40 lead-in bytes and a 24-byte gap after the TIB), each track
side verified straight after writing through the normal sector reads
following its TIB, bytes compared on every 10th track:

**3,486 of 3,486 sectors good (1,784,832 bytes), 0 CRC errors, 0 missing,
0 wrong, in 59.6 minutes** (most of that is the serial monitor; the writes
themselves take one revolution per track side). Log: `fulldisk-run1.log`.

The core's 14-byte lead-in was not enough here: sector 1 went missing on 2 of 4
track sides until the lead-in grew to 40 bytes and the TIB gap to 24.

On gardners' hint that sector writes need the data rate set explicitly: in
this exact core (release-0.97.2, 03b24c6), `F011WriteSectorRealDriveWait` waits
on `fdc_sector_found` from `mfm0`, whose rate is hard-wired to 81 cycles, so
`$D6A2` does not reach it. HD sector writes need that decoder to follow the
selected rate (or the wait to accept `fdc_sector_found_2x`).
