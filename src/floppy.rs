//! Write a DiskLab HD floppy (`Format::Hd`) to the MEGA65's internal drive.
//!
//! On release-0.97 cores the controller can format HD tracks and read their
//! sectors, but its sector write waits on the double-density decoder, so HD
//! sectors cannot be written one at a time. Instead each track side is built
//! here as raw flux, in the same layout as the core's own hardware format,
//! and a small routine on the MEGA65 (`launcher/floppywrite.a`) streams it to
//! the drive with DMA option `$0D` while the controller holds the write gate
//! open for one revolution, then reads every sector back to verify it.
//! Tracks 36-44 are written as 1581 double density so the ROM can boot the
//! disk; the rest at the standard HD rate.
use crate::{
    disk::{Disk, Format, Result, HD_DD_TRACKS},
    transfer::{Kind, Tool},
};
use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

pub const WRITER: &[u8] = include_bytes!("../launcher/floppywrite.bin");
const WRITER_AT: u32 = 0x1600;
const PARAMS: u32 = 0x1603; // cylinder, side, sectors, rate, done, tibok
const DONE: u32 = 0x1607;
const TIBOK: u32 = 0x1608;
const STATUS: u32 = 0x1609;
const LEN1: u32 = 0x1628;
const LEN2: u32 = 0x1639; // followed by src2 (3 bytes)
const FLUX_AT: u32 = 0x40000;
/// $D6A2 values: cycles of the 40.5 MHz clock per MFM cell.
pub const HD_RATE: u8 = 40;
pub const DD_RATE: u8 = 81;
/// DMA cycles added to every flux interval (measured on a MEGA65 R6).
const DMA_OVERHEAD: u32 = 3;
const LEAD_IN: usize = 40;
const TIB_GAP: usize = 24;
const REVOLUTION: u32 = 40_500_000 * 60 / 300;

fn crc16(data: &[u8], mut crc: u16) -> u16 {
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// MFM cells, two (clock, data) per data bit.
struct Cells {
    cells: Vec<u8>,
    prev: u8,
}
impl Cells {
    fn byte(&mut self, b: u8) {
        for i in (0..8).rev() {
            let d = b >> i & 1;
            self.cells.push(u8::from(self.prev == 0 && d == 0));
            self.cells.push(d);
            self.prev = d;
        }
    }
    fn bytes(&mut self, data: &[u8]) {
        data.iter().for_each(|b| self.byte(*b));
    }
    /// $A1 with the clock bit between data bits 4 and 5 missing ($4489).
    fn sync(&mut self) {
        for i in (0..16).rev() {
            self.cells.push((0x4489u16 >> i & 1) as u8);
        }
        self.prev = 1;
    }
}

pub fn is_dd(cylinder: u8) -> bool {
    HD_DD_TRACKS.contains(&(cylinder + 1))
}
pub fn rate(cylinder: u8) -> u8 {
    if is_dd(cylinder) {
        DD_RATE
    } else {
        HD_RATE
    }
}
/// Physical 512-byte sectors on one side of a cylinder.
pub fn sectors_per_side(cylinder: u8) -> usize {
    // Logical 256-byte sectors per track cover both sides.
    Format::Hd.sectors(cylinder + 1) / 4
}

/// The 512 bytes of physical sector `n` (from 1) of one track side.
pub fn physical_sector(disk: &Disk, cylinder: u8, side: u8, n: usize) -> Result<&[u8]> {
    let per_side = sectors_per_side(cylinder);
    let logical = side as usize * per_side * 2 + (n - 1) * 2;
    let p = disk.offset(cylinder + 1, logical as u8)?;
    Ok(&disk.bytes[p..p + 512])
}

/// One track side as MFM cells, laid out like the core's hardware format
/// (FDCAutoFormatTrack, with gaps), with a longer lead-in and TIB gap.
fn track_cells(disk: &Disk, cylinder: u8, side: u8) -> Result<(Vec<u8>, Vec<usize>)> {
    let rate = rate(cylinder);
    let count = sectors_per_side(cylinder);
    let mut c = Cells {
        cells: Vec::new(),
        prev: 0,
    };
    let mut gaps = Vec::new();
    c.bytes(&[0x00; LEAD_IN]);
    let tib = [0x65, cylinder, rate, 0x40, count as u8];
    (0..3).for_each(|_| c.sync());
    let crc = crc16(&tib, crc16(&[0xA1; 3], 0xFFFF));
    c.bytes(&tib);
    c.bytes(&crc.to_be_bytes());
    c.bytes(&[0x00; 4]);
    c.bytes(&[0x4E; TIB_GAP]);
    for n in 1..=count {
        let header = [0xFE, cylinder, side, n as u8, 0x02];
        (0..3).for_each(|_| c.sync());
        let crc = crc16(&header, crc16(&[0xA1; 3], 0xFFFF));
        c.bytes(&header);
        c.bytes(&crc.to_be_bytes());
        c.bytes(&[0x4E; 23]);
        c.bytes(&[0x00; 12]);
        (0..3).for_each(|_| c.sync());
        let data = physical_sector(disk, cylinder, side, n)?;
        let crc = crc16(data, crc16(&[0xA1, 0xA1, 0xA1, 0xFB], 0xFFFF));
        c.byte(0xFB);
        c.bytes(data);
        c.bytes(&crc.to_be_bytes());
        gaps.push(c.cells.len());
        c.bytes(&[0x4E; 24]);
    }
    Ok((c.cells, gaps))
}

/// DMA source bytes (one per flux transition, value = interval / 2) and the
/// index at which to split them into two chained jobs, inside a gap.
pub fn track_flux(disk: &Disk, cylinder: u8, side: u8) -> Result<(Vec<u8>, usize)> {
    let rate = rate(cylinder) as u32;
    let (cells, gaps) = track_cells(disk, cylinder, side)?;
    let mut flux = Vec::with_capacity(cells.len() / 2);
    let mut gap = 0u32;
    let mut cycles = 0u32;
    for &cell in &cells {
        gap += 1;
        if cell == 1 {
            // Rounded down, as in the tested Python prototype (track-at-once
            // proof and full-disk run): 80/120/160-cycle intervals at the HD
            // rate become 38/58/78.
            let v = ((gap * rate).saturating_sub(DMA_OVERHEAD) / 2).clamp(1, 255) as u8;
            cycles += 2 * v as u32 + DMA_OVERHEAD;
            flux.push(v);
            gap = 0;
        }
    }
    if cycles as f64 > REVOLUTION as f64 * 0.985 {
        return Err(format!(
            "Track {cylinder} side {side} is too long for one revolution."
        ));
    }
    let mut split = flux.len();
    if flux.len() > 0xFFFF {
        split = 0;
        for g in gaps {
            let middle = g + 12 * 16;
            let index = cells[..middle].iter().filter(|c| **c == 1).count();
            if index <= 0xFFFF {
                split = index;
            }
        }
        if split == 0 {
            return Err("No place to split the track's DMA jobs.".into());
        }
    }
    Ok((flux, split))
}

#[derive(Debug, Default)]
pub struct Report {
    pub track_sides: usize,
    pub sectors: usize,
    pub rewrites: usize,
    pub seconds: u64,
}

struct Monitor<'a> {
    tool: &'a Tool,
    device: Option<String>,
}
impl Monitor<'_> {
    fn run(&self, args: &[String]) -> Result<String> {
        let mut cmd = Command::new(&self.tool.path);
        cmd.args(args);
        if let Some(d) = &self.device {
            cmd.args(["--device", d]);
        }
        let out = cmd
            .output()
            .map_err(|e| format!("Cannot start m65rs: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "m65rs {}: {}",
                args.first().map(String::as_str).unwrap_or(""),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
    fn peek(&self, addr: u32) -> Result<u8> {
        let out = self.run(&["peek".into(), format!("{addr:x}")])?;
        let v = out.rsplit('$').next().unwrap_or("").trim();
        u8::from_str_radix(v, 16).map_err(|_| format!("Unexpected m65rs output: {out}"))
    }
    fn poke(&self, addr: u32, bytes: &[u8]) -> Result<()> {
        let mut args = vec!["poke".to_string(), format!("{addr:x}")];
        args.extend(bytes.iter().map(|b| format!("{b:02x}")));
        self.run(&args).map(|_| ())
    }
    fn dump(&self, addr: u32, len: usize) -> Result<Vec<u8>> {
        let out = self.run(&["dump".into(), format!("{addr:x}"), len.to_string()])?;
        let mut v = Vec::new();
        for line in out.lines().filter_map(|l| l.split_once(':').map(|x| x.1)) {
            let hex = line.trim_start().split("  ").next().unwrap_or("");
            v.extend(
                hex.split_whitespace()
                    .filter_map(|h| u8::from_str_radix(h, 16).ok()),
            );
        }
        v.truncate(len);
        Ok(v)
    }
    fn load(&self, file: &Path, addr: u32) -> Result<()> {
        self.run(&["load".into(), format!("{}@{addr:x}", file.display())])
            .map(|_| ())
    }
    fn idle(&self) -> Result<()> {
        let end = Instant::now() + Duration::from_secs(3);
        while self.peek(0xFFD3082)? & 0x80 != 0 {
            if Instant::now() > end {
                return Err("The floppy controller stayed busy.".into());
            }
        }
        Ok(())
    }
}

/// Write and verify the whole disk. The MEGA65 must be at READY with a
/// writable HD disk in drive 0. `progress(done, total, message)`.
pub fn write_disk(
    tool: &Tool,
    device: Option<String>,
    disk: &Disk,
    work: &Path,
    mut progress: impl FnMut(usize, usize, &str),
) -> Result<Report> {
    if disk.format != Format::Hd {
        return Err("Only DiskLab HD floppy images (.dhd) can be written to a floppy.".into());
    }
    if tool.kind != Kind::M65rs {
        return Err("Writing a floppy needs m65rs (MEGA65 Studio).".into());
    }
    let m = Monitor { tool, device };
    let started = Instant::now();
    let writer = work.join("floppywrite.bin");
    std::fs::write(&writer, WRITER).map_err(|e| e.to_string())?;
    // Halt whatever is running (an autobooting START loads its reader at
    // the same address) so nothing overwrites the writer.
    m.run(&["stop".into()])?;
    m.load(&writer, WRITER_AT)?;
    const IO: u32 = 0xFFD3000;
    m.poke(IO + 0x6A1, &[0x01])?; // real drive 0
    m.poke(IO + 0x696, &[0x00])?; // no automatic seeking while writing
    m.poke(IO + 0x080, &[0x68])?; // motor on, side 0
    std::thread::sleep(Duration::from_millis(800));
    if m.peek(IO + 0x082)? & 0x02 != 0 {
        return Err("The floppy is write-protected.".into());
    }
    if m.peek(IO + 0x083)? & 0x08 == 0 {
        return Err("No disk in the drive.".into());
    }
    for _ in 0..90 {
        if m.peek(IO + 0x082)? & 0x01 != 0 {
            break;
        }
        m.poke(IO + 0x081, &[0x10])?;
        m.idle()?;
    }
    if m.peek(IO + 0x082)? & 0x01 == 0 {
        return Err("Could not find track 0.".into());
    }
    let cylinders = Format::Hd.tracks();
    let total = cylinders as usize * 2;
    let mut report = Report::default();
    let flux_file: PathBuf = work.join("track.bin");
    for cyl in 0..cylinders {
        if cyl > 0 {
            m.poke(IO + 0x081, &[0x18])?;
            m.idle()?;
        }
        for side in 0..2u8 {
            let done_sides = cyl as usize * 2 + side as usize;
            let (flux, split) = track_flux(disk, cyl, side)?;
            std::fs::write(&flux_file, &flux).map_err(|e| e.to_string())?;
            m.load(&flux_file, FLUX_AT)?;
            let rest = flux.len() - split;
            let src2 = FLUX_AT + split as u32;
            m.poke(LEN1, &(split as u16).to_le_bytes())?;
            m.poke(
                LEN2,
                &[
                    rest as u8,
                    (rest >> 8) as u8,
                    src2 as u8,
                    (src2 >> 8) as u8,
                    (src2 >> 16) as u8,
                ],
            )?;
            let count = sectors_per_side(cyl);
            let mut attempt = 0;
            loop {
                progress(
                    done_sides,
                    total,
                    &format!(
                        "Track {} side {side}{}",
                        cyl,
                        if attempt > 0 { " (again)" } else { "" }
                    ),
                );
                m.poke(PARAMS, &[cyl, side, count as u8, rate(cyl), 0, 0])?;
                m.run(&["go".into(), format!("{WRITER_AT:x}")])?;
                let end = Instant::now() + Duration::from_secs(20);
                while m.peek(DONE)? != 1 {
                    if Instant::now() > end {
                        return Err(format!(
                            "The MEGA65 did not finish track {cyl} side {side}."
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                let status = m.dump(STATUS, count)?;
                let bad: Vec<usize> = status
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| **s & 0x18 != 0)
                    .map(|(i, _)| i + 1)
                    .collect();
                let tib = m.peek(TIBOK)? == 1;
                if bad.is_empty() && tib {
                    report.sectors += count;
                    break;
                }
                attempt += 1;
                report.rewrites += 1;
                if attempt == 3 {
                    return Err(format!(
                        "Track {cyl} side {side} does not verify (sectors {bad:?}{}). Try another disk.",
                        if tib { "" } else { ", no track info" }
                    ));
                }
            }
            report.track_sides += 1;
        }
    }
    m.poke(IO + 0x080, &[0x00])?;
    report.seconds = started.elapsed().as_secs();
    progress(total, total, "Done");
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tracks_fit_and_split_inside_gaps() {
        let mut disk = crate::launcher::create_floppy("T", "65").unwrap();
        disk.put("DATA", 1, &vec![0x33; 1_000_000], false).unwrap();
        for cyl in [0u8, 34, 35, 39, 43, 44, 82] {
            for side in 0..2 {
                let (flux, split) = track_flux(&disk, cyl, side).unwrap();
                assert!(split <= 0xFFFF && flux.len() - split <= 0xFFFF);
                let expect = if is_dd(cyl) { 10 } else { 21 };
                assert_eq!(sectors_per_side(cyl), expect);
            }
        }
        // Physical sector n of a side holds logical sectors 2(n-1), 2(n-1)+1.
        let p = disk.offset(5, 42 + 2).unwrap();
        assert_eq!(
            physical_sector(&disk, 4, 1, 2).unwrap(),
            &disk.bytes[p..p + 512]
        );
        assert_eq!(crc16(&[0xA1, 0xA1, 0xA1, 0xFE, 0, 0, 1, 2], 0xFFFF), 0xCA6F);
    }
}
