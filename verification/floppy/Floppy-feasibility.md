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
