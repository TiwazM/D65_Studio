# MEGA65 DiskLab

**A cross-platform disk-image workbench for the MEGA65.**

MEGA65 DiskLab is a native application written in Rust for creating, opening, inspecting and editing Commodore D81 and MEGA65 D65 disk images. It also includes an **experimental D65 compilation builder and resident MEGA65 file reader** that can launch programs from storage beyond the area supported by the standard ROM.

The project began with a simple question: *Why is it so difficult to create and fill a D65 image on a computer?* It has since grown into an experiment in larger disk-image storage and compatibility with existing MEGA65 software.

> **Status: v0.3.1 — experimental preview.** Some functions have been tested on a physical MEGA65; others are still under development. **Do not trust the direct SD-card writer with your only copy of a card.**

## Features

- Native **Windows x64** and **macOS** builds; no Python or Rust installation needed to run the application.
- Create and open **D81 and D65** images; inspect directories and file contents.
- Add, extract, rename and delete **PRG, SEQ and USR** files; any host file can be added, and adding a D81 or D65 copies all of its files in.
- Convert supported D65/D81 container layouts, refusing conversions that would lose extended data.
- Browse mounted images in **Windows Explorer or macOS Finder** using a local WebDAV bridge; no separate filesystem driver required.
- Merge programs and supporting files from multiple D81s into a **read-only D65 compilation** with a `START` launcher.
- Create an empty **Large D65 with START resident reader** and fill it directly: adding, renaming or deleting a file repacks the image and rebuilds `START` and its catalogue.
- Pack about **5.4 MB of shared launcher-and-file payload** into a compilation: the resident reader reads the mounted image's SD sectors directly, so the whole 5,570,560-byte container is usable.
- Launch compatible MEGA65 programs and use a **resident, read-only file-reader API** for further file reads.
- Optional **experimental KERNAL LOAD/VERIFY compatibility** for certain existing programs on ROM 920413, including BASIC `BLOAD`.
- Backups, source-file verification and checks intended to detect corrupt or unsupported images.

## Quick start

1. Run MEGA65 DiskLab on Windows or macOS.
2. Choose **File › New D65…** (or New D81…) or **File › Open Disk Image…** to work with a standard D81/D65 image. Everything else lives in the File, Edit, Disk and View menus; edits are saved as you make them.
3. Use the file list to add, inspect, extract or rename files. Standard-image changes are saved automatically; a backup is made before the first edit of a session.
4. For a larger compilation, choose **File › Merge D81s…**, select the source images and enable **Include START launcher**.
5. Optionally enable **Experimental LOAD compatibility (ROM 920413)** for programs using supported standard loading calls.
6. Save the compilation as `.d65`, copy it to an SD card, mount it as drive 8 on the MEGA65, and enter:

```basic
RUN"START"
```

Select a program from the menu.

A resident image can carry `AUTOBOOT.C65`, a one-line program that BASIC 65 runs from drive 8 after every reset, so the START menu appears by itself while the image is mounted: tick it when creating a large disk, toggle it under Edit, or run `mega65-disklab autoboot IMAGE on|off`.

To build a large disk from scratch instead, choose **New disk**, keep **D65** selected and tick **Large D65 with START resident reader**. The new image holds only `START`; add PRG, SEQ and USR files with **Add files…**. Every change repacks the whole image, regenerates the `START` menu (BASIC programs at `$2001`, up to 99) and keeps a backup of the original beside it. The same is available from the command line:

```sh
mega65-disklab create d65big BIG.d65 "MY BIG DISK"
mega65-disklab add BIG.d65 GAME.prg GAME
```

Compilations built with **Merge D81s…** can be edited the same way: adding, renaming or deleting a file repacks the image and rebuilds `START`. They stay read-only when browsed through Explorer/Finder. When a newer MEGA65 DiskLab ships a changed resident reader, the sidebar says the image's `START` is older than the app; **Update START** (or `mega65-disklab update-start IMAGE`) replaces `START` and the catalogue while leaving every file byte for byte as it was, so there is no need to rebuild or re-export a whole disk for a reader fix. A backup of the original is kept beside the image.

## How the experimental loader works

The MEGA65 D65 image container is **5,570,560 bytes** (85 tracks × 256 logical sectors), but the standard D81-compatible filesystem only allocates 40 sectors per track, and ROM **920413** does not ordinarily read files in the higher sectors used by the experimental compilation. The F011 floppy-controller emulation in core 0.97.2 is a further limit: for D65 images it clamps physical sectors to 0–63 and tracks to 1–84 (`sdcardio.vhdl`), so track/sector reads can never reach more than about 2.7 MB of the image.

MEGA65 DiskLab addresses this with a small `START` program and a custom resident reader that bypasses the F011 path. Hyppo only mounts a D65 whose FAT clusters are contiguous and publishes the image's first SD sector at `$D68C-$D68F`, so the reader computes each 512-byte image sector's SD address, reads it with the SD controller (`$D680-$D684`) and copies the wanted 256-byte half from the sector buffer at `$FFD6E00` by DMA. Every logical sector on tracks 1–85 is therefore reachable. The reader keeps a small entry routine in the documented utility-memory region and uses reserved Attic RAM for its catalogue and read buffers.

| Current property | Status |
| --- | --- |
| D65 container | 5,570,560 bytes |
| Experimental compilation capacity | About 5.4 MB (21,504 blocks of 254 bytes) shared by `START` and files |
| Supported menu PRG size | Up to 55,041 bytes including load address |
| Menu entries | Up to 99 compatible programs |
| Other imported files | PRG, SEQ and USR, subject to limits |
| Resident API | Read-only; callable by cooperating applications |
| Automatic `LOAD` / `BLOAD` support | Experimental, limited to supported calls on ROM 920413 |
| `OPEN` streams, custom loaders, disk writes | Not supported |
| Hardware validation | The direct-SD reader passes the CPU/SD harness, and on a MEGA65 R6 (core 0.97.2, ROM 920413) it loaded and ran a 55,041-byte program stored on logical sectors 128–255 of tracks 21–35 of a full-size image written by the macOS SD export |

### Experimental compatibility mode

When enabled, supported standard KERNAL `LOAD`/`VERIFY` requests to **drive 8** are redirected through the resident reader. The goal is to let existing programs load additional files without modifying their source code. This is **not** a universal replacement DOS:

- `OPEN`/streamed reads and `CLOSE` handling are not yet redirected.
- Programs with custom sector loaders may bypass the hooks.
- Programs that replace vectors, alter memory mappings or overwrite reserved memory may break the resident reader.
- C64 mode and write operations are not supported by the compatibility layer.
- Programs using other devices normally pass through to the original routines.

Technical details: [Resident driver ABI](launcher/Resident-driver.txt) · [LOAD compatibility](launcher/LOAD-compatibility.txt) · [Disk format notes](FORMAT.md).

## Real-hardware results

Tests have been performed on a **physical MEGA65 running core 0.97.2 and ROM 920413**, in addition to emulator and automated checks.

**Confirmed during development:**

- An experimental D65 compilation mounted and launched real programs from additional sectors, including IONDRIFT and ROCKSTORM.
- The **HOOKBAS** demonstration ran on hardware and displayed **`BASIC BLOAD SECOND FILE OK`**, confirming a second-file read through the experimental hook in that test.
- A larger compilation of approximately **2.19 MB** mounted and displayed its launcher on hardware. Multiple programs ran; Turtle65 was also observed loading example files. This does **not** establish universal compatibility or prove every example file's physical sector position.

**Still experimental:**

- Not all programs work; programs with custom loading or unusual memory use require case-by-case investigation.
- The Windows **direct SD-card export** has shown failures/unmounting issues in testing. Generating a D65 and manually copying it from Windows succeeded for a tested image, but ordinary copying does **not guarantee** contiguity.
- SD-card image placement matters: MEGA65 developers have advised that mounted disk-image files must occupy **contiguous FAT clusters**. Identical image bytes can behave differently if stored differently on the card.
- The direct SD-card writers and their recovery mechanisms need further real-card testing before they should be considered reliable.

These are observed results, not a blanket claim that every D65 image, SD card, ROM or application will work.

## Sending an image to the MEGA65

**File › Send to MEGA65** uploads the open image through the MEGA65 itself, over the USB JTAG adapter's serial monitor or over Ethernet, using a copy of `m65rs` (MEGA65 Studio / mega65-jtag-rs) or `mega65_ftp` (mega65-tools, bundled with M65Connect) found on this computer. The MEGA65 writes the file to its own SD card in one contiguous run, so no card is unmounted or opened raw on the computer and no administrator prompt appears. The cluster chain is read back afterwards and the transfer fails loudly if the file is not contiguous. On 17 October 2026 a full-size image sent this way over JTAG landed in one run, mounted, and ran its last menu entry from the high sectors. The dialog can also ask the Hypervisor to mount the image as drive 8 and then type `RUN"START"` on the MEGA65 (`--run` on the command line; needs `m65rs` over the JTAG port). The MEGA65 must be at the READY prompt; the transfer helper replaces whatever program is in its memory. The same from the command line:

```sh
mega65-disklab send GAME.d65 GAME.D65 --mount
mega65-disklab send GAME.d65 GAME.D65 --ethernet
```

`--serial PORT`, `--ethernet IPV6` and `--tool PATH` pick the port, address and program explicitly. **File › Preferences…** stores the paths of `m65rs` and `mega65_ftp` when they are not found automatically (PATH, MEGA65 Studio, M65Connect, or the `D65_M65RS` / `D65_MEGA65_FTP` variables). Both tools allocate one contiguous run (`m65rs` from its 18 October 2026 build on; an older `m65rs` fragments, which the read-back catches). Over the JTAG port `m65rs` is preferred because it also reads the chain back; Ethernet needs `mega65_ftp`. A fragmented result is deleted again and reported. Both paths were tested on a MEGA65 R6 with a full-size image: one run, mounted, last menu entry ran from the high sectors. The direct SD-card export below remains available when no MEGA65 is connected.

## SD-card export: caution

MEGA65 DiskLab has experimental tools for writing an image as a **contiguous FAT32 file**. These are different from ordinary file copying. Their low-level operation may temporarily unmount the card and request administrator privileges.

**Use a spare, backed-up SD card for testing.** Never disconnect the card while a raw export is in progress. The implementation performs validation and keeps recovery records, but a power loss or unexpected driver behaviour can still damage the filesystem.

If an export fails, **do not assume the card was restored or that the resulting image is usable**. Inspect the reported error and recovery instructions. On Windows, the direct-export path has known issues; a manual copy has worked in some tests but remains allocation-dependent.

See [Windows instructions](WINDOWS.txt) for Windows-specific caveats and [verification reports](verification) for tests and diagnostics.

## Development and builds

The disk-format engine, compilation builder and resident loader packaging are shared Rust code. Operating-system integration (volume mounting, removable-card handling and privileged raw access) is platform-specific.

With a Rust toolchain installed:

```sh
cargo test --locked
cargo build --locked --release
```

For macOS universal-build instructions, see `build-macos.sh`. For Windows startup and behaviour, see [WINDOWS.txt](WINDOWS.txt).

Some CLI operations are available through the compiled application; see the existing project documentation for the supported command list.

## Contributing

Contributions, testing and corrections are very welcome — particularly from developers familiar with the MEGA65 FDC, CBDOS, KERNAL vectors and FAT32 allocation.

Useful areas include **fuller D65 capacity**, compatibility with more programs and ROM versions, handling of secondary files and custom loaders, Windows/macOS SD-card robustness, and improved documentation.

For reports, please include the MEGA65 DiskLab version, operating system, MEGA65 core and ROM versions, exact reproduction steps and any error text. A disposable test image is preferable to one containing personal files.

## Credits and transparency

Created by **Tiwaz / TWS Software Studios** with **AI assistance**, extensive iterative testing, and input from discussions in the MEGA65 community. Thanks to community members who shared advice about D65 layouts, controller access and SD-card image allocation.

The project is an **unofficial community tool**, not an official MEGA65 release.

## License

A license has not been specified in this README. Add the repository's chosen `LICENSE` file and confirm any included dependencies, firmware-derived assets or third-party components are compatible before publishing.
