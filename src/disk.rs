//! D81 filesystem in either D81 or MEGA65 Freezer D65 geometry.
//! See FORMAT.md for primary-source references and deliberately bounded allocation.
use std::collections::HashSet;
pub type Result<T> = std::result::Result<T, String>;
pub const D81_SIZE: usize = 819_200;
pub const D65_SIZE: usize = 5_570_560;
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Format {
    D81,
    D65,
}
impl Format {
    pub fn stride(self) -> usize {
        if self == Self::D81 {
            40
        } else {
            256
        }
    }
    pub fn size(self) -> usize {
        if self == Self::D81 {
            D81_SIZE
        } else {
            D65_SIZE
        }
    }
    pub fn ext(self) -> &'static str {
        if self == Self::D81 {
            "d81"
        } else {
            "d65"
        }
    }
}
#[derive(Clone)]
pub struct Disk {
    pub bytes: Vec<u8>,
    pub format: Format,
}
#[derive(Clone, Debug)]
pub struct Entry {
    pub offset: usize,
    pub name: String,
    pub raw_name: [u8; 16],
    pub kind: u8,
    pub flags: u8,
    pub blocks: usize,
    pub start: (u8, u8),
}
impl Entry {
    pub fn type_name(&self) -> &'static str {
        match self.kind {
            1 => "SEQ",
            2 => "PRG",
            3 => "USR",
            4 => "REL",
            5 => "CBM",
            _ => "???",
        }
    }
    pub fn host_name(&self) -> String {
        let mut s = String::new();
        for b in self.raw_name.iter().copied().take_while(|b| *b != 0xa0) {
            if (32..=95).contains(&b) && !b"/\\:%?*\"<>|".contains(&b) {
                s.push(b as char)
            } else {
                s.push_str(&format!("%{b:02X}"))
            }
        }
        if s.is_empty() {
            s.push_str("%EMPTY")
        }
        format!("{}.{}", s, self.type_name().to_lowercase())
    }
}
pub fn encode_name(name: &str, max: usize) -> Result<Vec<u8>> {
    let name = name.trim().to_ascii_uppercase();
    if name.is_empty()
        || name.len() > max
        || !name
            .bytes()
            .all(|b| (32..=95).contains(&b) && !b"\"*?,:/\\%".contains(&b))
    {
        return Err(format!(
            "Use 1–{max} plain ASCII characters; no quotes, wildcards, slashes, colon or %."
        ));
    }
    Ok(name.into_bytes())
}
pub fn display_name(bytes: &[u8]) -> String {
    bytes
        .iter()
        .take_while(|b| **b != 0xa0)
        .map(|b| match b {
            32..=95 => *b as char,
            193..=218 => (*b - 128) as char,
            _ => '�',
        })
        .collect()
}
impl Disk {
    /// Experimental gardners-style bank merge. The current core clamps physical
    /// sectors above 63, so accept at most three banks (logical sectors 0..119).
    /// The preserved BAMs are not a standardized extended allocation map.
    pub fn merge_d81(sources: &[Self]) -> Result<Self> {
        if !(2..=3).contains(&sources.len()) {
            return Err(
                "Choose two or three D81s. Larger merges exceed the verified controller bounds."
                    .into(),
            );
        }
        let mut names = HashSet::new();
        let mut expected = Vec::new();
        for source in sources {
            if source.format != Format::D81 {
                return Err("Every merge source must be a D81 image.".into());
            }
            source.validate_writable()?;
            for e in source.entries()? {
                if !names.insert(e.raw_name) {
                    return Err(format!(
                        "Duplicate name {}. Rename it in a source D81 first.",
                        e.name
                    ));
                }
                expected.push((e.raw_name, e.flags, e.blocks, source.read(&e)?));
            }
        }
        let mut d = sources[0].convert(Format::D65)?;
        let first_dirs = sources[0].directory_chain()?;
        let mut tail = *first_dirs.last().ok_or("Missing first directory")?;
        for (bank, source) in sources.iter().enumerate().skip(1) {
            let shift = (bank * 40) as u8;
            for t in 1..=80 {
                let from = source.offset(t, 0)?;
                let to = d.offset(t, shift)?;
                d.bytes[to..to + 40 * 256].copy_from_slice(&source.bytes[from..from + 40 * 256]);
            }
            // Shift actual links only. A terminal sector's second byte is a
            // byte count, NOT a sector number, and must not be adjusted.
            for e in source.entries()? {
                for (t, s) in source.chain(&e)? {
                    let p = d.offset(t, s + shift)?;
                    if d.bytes[p] != 0 {
                        d.bytes[p + 1] += shift;
                    }
                }
            }
            let dirs = source.directory_chain()?;
            for &(t, s) in &dirs {
                let from = source.offset(t, s)?;
                let to = d.offset(t, s + shift)?;
                if d.bytes[to] != 0 {
                    d.bytes[to + 1] += shift;
                }
                for i in 0..8 {
                    let slot = from + i * 32;
                    if source.bytes[slot + 2] & 7 != 0 && source.bytes[slot + 3] != 0 {
                        d.bytes[to + i * 32 + 4] += shift;
                    }
                }
            }
            // Keep each source header/BAM as provenance, relocating its links.
            // No ROM support for using these extra BAMs is assumed.
            for s in [0, 1] {
                let p = d.offset(40, s + shift)?;
                if d.bytes[p] != 0 {
                    d.bytes[p + 1] += shift;
                }
            }
            let p = d.offset(tail.0, tail.1)?;
            d.bytes[p] = dirs[0].0;
            d.bytes[p + 1] = dirs[0].1 + shift;
            let &(t, s) = dirs.last().ok_or("Missing source directory")?;
            tail = (t, s + shift);
        }
        let merged = d.entries()?;
        if merged.len() != expected.len() {
            return Err("Merge verification failed: directory length changed.".into());
        }
        for ((name, flags, blocks, bytes), output) in expected.iter().zip(&merged) {
            if *name != output.raw_name
                || *flags != output.flags
                || *blocks != output.blocks
                || *bytes != d.read(output)?
            {
                return Err(format!("Merge verification failed for {}.", output.name));
            }
        }
        Ok(d)
    }
    pub fn parse(bytes: Vec<u8>) -> Result<Self> {
        let format = match bytes.len() {
            D81_SIZE => Format::D81,
            D65_SIZE => Format::D65,
            n => {
                return Err(format!(
                "Unsupported image size: {n} bytes. Expected D81 (819200) or MEGA65 D65 (5570560)."
            ))
            }
        };
        let d = Self { bytes, format };
        let h = d.offset(40, 0)?;
        if d.bytes[h + 2] != 0x44 {
            return Err(
                "Unrecognized DOS header. This app supports Freezer/D81-style filesystems only."
                    .into(),
            );
        }
        d.entries()?;
        Ok(d)
    }
    pub fn create(format: Format, label: &str, id: &str) -> Result<Self> {
        let mut d = Self {
            bytes: vec![0; format.size()],
            format,
        };
        let label = encode_name(label, 16)?;
        let id = encode_name(id, 2)?;
        if id.len() != 2 {
            return Err("Disk ID must be exactly two characters.".into());
        }
        let h = d.offset(40, 0)?;
        d.bytes[h..h + 4].copy_from_slice(&[40, 3, 0x44, 0]);
        d.bytes[h + 4..h + 29].fill(0xa0);
        d.bytes[h + 4..h + 4 + label.len()].copy_from_slice(&label);
        d.bytes[h + 22..h + 24].copy_from_slice(&id);
        d.bytes[h + 25..h + 27].copy_from_slice(b"1D");
        for bs in 1..=2 {
            let o = d.offset(40, bs)?;
            d.bytes[o..o + 8].copy_from_slice(&[
                if bs == 1 { 40 } else { 0 },
                if bs == 1 { 2 } else { 255 },
                0x44,
                0xbb,
                id[0],
                id[1],
                0xc0,
                0,
            ]);
            for i in 0..40 {
                let p = o + 16 + i * 6;
                d.bytes[p] = 40;
                d.bytes[p + 1..p + 6].fill(255);
            }
        }
        for s in 0..4 {
            d.set_free(40, s, false)?;
        }
        let dir = d.offset(40, 3)?;
        d.bytes[dir + 1] = 255;
        Ok(d)
    }
    pub fn offset(&self, t: u8, s: u8) -> Result<usize> {
        let tracks = if self.format == Format::D81 { 80 } else { 85 };
        if t == 0 || t as usize > tracks || s as usize >= self.format.stride() {
            return Err(format!("Invalid track/sector {t}/{s}."));
        }
        Ok(((t as usize - 1) * self.format.stride() + s as usize) * 256)
    }
    pub fn label(&self) -> String {
        display_name(&self.bytes[self.offset(40, 0).unwrap() + 4..][..16])
    }
    pub fn disk_id(&self) -> String {
        display_name(&self.bytes[self.offset(40, 0).unwrap() + 22..][..2])
    }
    fn bam(&self, t: u8) -> Result<usize> {
        if !(1..=80).contains(&t) {
            return Err("Track is outside the supported allocation map.".into());
        }
        Ok(self.offset(40, if t <= 40 { 1 } else { 2 })? + 16 + ((t as usize - 1) % 40) * 6)
    }
    fn is_free(&self, t: u8, s: u8) -> Result<bool> {
        if s >= 40 {
            return Err("Extended D65 allocation is not yet standardized in this app.".into());
        }
        Ok(self.bytes[self.bam(t)? + 1 + (s / 8) as usize] & (1 << (s % 8)) != 0)
    }
    fn set_free(&mut self, t: u8, s: u8, free: bool) -> Result<()> {
        let p = self.bam(t)?;
        if s >= 40 {
            return Err("Unsupported allocation sector.".into());
        }
        if self.is_free(t, s)? != free {
            if free {
                self.bytes[p] += 1
            } else {
                self.bytes[p] -= 1
            }
        }
        let b = &mut self.bytes[p + 1 + (s / 8) as usize];
        if free {
            *b |= 1 << (s % 8)
        } else {
            *b &= !(1 << (s % 8))
        }
        Ok(())
    }
    pub fn free_blocks(&self) -> usize {
        (1..=80)
            .filter(|t| *t != 40)
            .map(|t| self.bytes[self.bam(t).unwrap()] as usize)
            .sum()
    }
    pub fn directory_chain(&self) -> Result<Vec<(u8, u8)>> {
        let h = self.offset(40, 0)?;
        let (mut t, mut s) = (self.bytes[h], self.bytes[h + 1]);
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        while t != 0 {
            if !seen.insert((t, s)) {
                return Err("Directory chain contains a cycle.".into());
            }
            let p = self.offset(t, s)?;
            out.push((t, s));
            t = self.bytes[p];
            s = self.bytes[p + 1];
        }
        if out.is_empty() {
            return Err("Missing directory chain.".into());
        }
        Ok(out)
    }
    pub fn entries(&self) -> Result<Vec<Entry>> {
        let mut out = Vec::new();
        for (t, s) in self.directory_chain()? {
            let p = self.offset(t, s)?;
            for i in 0..8 {
                let o = p + i * 32;
                if self.bytes[o + 2] & 7 == 0 {
                    continue;
                }
                let raw_name: [u8; 16] = self.bytes[o + 5..o + 21].try_into().unwrap();
                out.push(Entry {
                    offset: o,
                    name: display_name(&raw_name),
                    raw_name,
                    kind: self.bytes[o + 2] & 7,
                    flags: self.bytes[o + 2],
                    blocks: u16::from_le_bytes([self.bytes[o + 30], self.bytes[o + 31]]) as usize,
                    start: (self.bytes[o + 3], self.bytes[o + 4]),
                });
            }
        }
        Ok(out)
    }
    /// Launcher images keep all directory metadata in the ROM-readable bank.
    /// Payload start pointers can still refer to the extended sectors.
    /// Allocation use of a resident compilation, including START, in 254-byte blocks.
    pub fn resident_space(&self) -> Option<(usize, usize)> {
        let h = self.offset(40, 0).ok()?;
        if self.format != Format::D65 || self.bytes[h + 48..h + 56] != *b"D65RES01" {
            return None;
        }
        let mut used = HashSet::new();
        for e in self.entries().ok()? {
            for (t, s) in self.chain(&e).ok()? {
                if t == 40 || s >= 120 || !used.insert((t, s)) {
                    return None;
                }
            }
        }
        Some((used.len(), 79 * 120))
    }
    pub fn chain(&self, e: &Entry) -> Result<Vec<(u8, u8)>> {
        let (mut t, mut s) = e.start;
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        while t != 0 {
            if !seen.insert((t, s)) {
                return Err(format!("File {} has a cyclic chain.", e.name));
            }
            let o = self.offset(t, s)?;
            out.push((t, s));
            t = self.bytes[o];
            s = self.bytes[o + 1];
            if t == 0 && s == 0 {
                return Err("Invalid last-sector byte count.".into());
            }
        }
        Ok(out)
    }
    pub fn read(&self, e: &Entry) -> Result<Vec<u8>> {
        if !(1..=3).contains(&e.kind) {
            return Err("Only SEQ, PRG and USR files can be extracted or edited.".into());
        }
        let mut data = Vec::new();
        for (t, s) in self.chain(e)? {
            let o = self.offset(t, s)?;
            let n = if self.bytes[o] == 0 {
                self.bytes[o + 1] as usize - 1
            } else {
                254
            };
            data.extend_from_slice(&self.bytes[o + 2..o + 2 + n]);
        }
        Ok(data)
    }
    /// Fail closed on structures whose allocation semantics we cannot safely update.
    pub fn validate_writable(&self) -> Result<()> {
        let header = self.offset(40, 0)?;
        if self.bytes[header + 48..header + 56] == *b"D65RES01" {
            return Err("Resident-reader compilation: read-only. Rebuild from the source D81 images to change its contents.".into());
        }
        let mut occupied = HashSet::from([(40, 0), (40, 1), (40, 2)]);
        let id = self.offset(40, 0)? + 22;
        for bs in 1..=2 {
            let p = self.offset(40, bs)?;
            if self.bytes[p + 2..p + 4] != [0x44, 0xbb]
                || self.bytes[p + 4..p + 6] != self.bytes[id..id + 2]
            {
                return Err("BAM signature or disk ID mismatch; image is read-only.".into());
            }
        }
        for t in 1..=80 {
            let p = self.bam(t)?;
            let n: u32 = self.bytes[p + 1..p + 6]
                .iter()
                .map(|v| v.count_ones())
                .sum();
            if n != self.bytes[p] as u32 {
                return Err(format!(
                    "BAM free count mismatch on track {t}; image is read-only."
                ));
            }
        }
        for ts in self.directory_chain()? {
            if ts.0 != 40 || ts.1 < 3 || ts.1 >= 40 || !occupied.insert(ts) {
                return Err("Nonstandard directory allocation; image is read-only.".into());
            }
        }
        let mut names = HashSet::new();
        for e in self.entries()? {
            if !(1..=3).contains(&e.kind) || e.flags & 0x80 == 0 {
                return Err(format!(
                    "{} is an unsupported or unclosed file; image is read-only.",
                    e.name
                ));
            }
            if !names.insert(e.raw_name) {
                return Err("Duplicate directory filenames; image is read-only.".into());
            }
            let chain = self.chain(&e)?;
            if chain.len() != e.blocks {
                return Err(format!(
                    "Block count mismatch for {}; image is read-only.",
                    e.name
                ));
            }
            for ts in chain {
                if ts.0 > 80 || ts.1 >= 40 || ts.0 == 40 || !occupied.insert(ts) {
                    return Err(
                        "Extended, reserved or cross-linked file sectors; image is read-only."
                            .into(),
                    );
                }
            }
        }
        for (t, s) in occupied {
            if self.is_free(t, s)? {
                return Err("Referenced sector is marked free; image is read-only.".into());
            }
        }
        Ok(())
    }
    fn find(&self, name: &[u8]) -> Result<Option<Entry>> {
        Ok(self.entries()?.into_iter().find(|e| {
            e.raw_name
                .iter()
                .copied()
                .take_while(|b| *b != 0xa0)
                .collect::<Vec<_>>()
                == name
        }))
    }
    pub fn put(&mut self, name: &str, kind: u8, data: &[u8], replace: bool) -> Result<()> {
        self.validate_writable()?;
        let name = encode_name(name, 16)?;
        if !(1..=3).contains(&kind) {
            return Err("Choose SEQ, PRG or USR.".into());
        }
        let existing = self.find(&name)?;
        if existing.is_some() && !replace {
            return Err("A file with this name already exists.".into());
        }
        let mut d = self.clone();
        let slot = if let Some(e) = existing {
            if e.flags & 0x40 != 0 {
                return Err("File is locked.".into());
            }
            d.remove_inner(&e)?;
            e.offset
        } else {
            d.empty_slot()?
        };
        let needed = std::cmp::max(1, data.len().div_ceil(254));
        let mut free = Vec::new();
        for t in (1..40).rev().chain(41..=80) {
            for s in 0..40 {
                if d.is_free(t, s)? {
                    free.push((t, s));
                    if free.len() == needed {
                        break;
                    }
                }
            }
            if free.len() == needed {
                break;
            }
        }
        if free.len() != needed {
            return Err("Disk full. The supported filesystem has 790 KiB of data sectors.".into());
        }
        for (i, &(t, s)) in free.iter().enumerate() {
            d.set_free(t, s, false)?;
            let p = d.offset(t, s)?;
            d.bytes[p..p + 256].fill(0);
            let chunk = &data[(i * 254).min(data.len())..((i + 1) * 254).min(data.len())];
            if let Some(&(nt, ns)) = free.get(i + 1) {
                d.bytes[p] = nt;
                d.bytes[p + 1] = ns;
            } else {
                d.bytes[p + 1] = (chunk.len() + 1) as u8;
            }
            d.bytes[p + 2..p + 2 + chunk.len()].copy_from_slice(chunk);
        }
        d.bytes[slot + 2..slot + 32].fill(0);
        d.bytes[slot + 2] = 0x80 | kind;
        d.bytes[slot + 3] = free[0].0;
        d.bytes[slot + 4] = free[0].1;
        d.bytes[slot + 5..slot + 21].fill(0xa0);
        d.bytes[slot + 5..slot + 5 + name.len()].copy_from_slice(&name);
        d.bytes[slot + 30..slot + 32].copy_from_slice(&(needed as u16).to_le_bytes());
        d.validate_writable()?;
        *self = d;
        Ok(())
    }
    fn empty_slot(&mut self) -> Result<usize> {
        let dirs = self.directory_chain()?;
        for &(t, s) in &dirs {
            let p = self.offset(t, s)?;
            for i in 0..8 {
                if self.bytes[p + i * 32 + 2] & 7 == 0 {
                    return Ok(p + i * 32);
                }
            }
        }
        for s in 4..40 {
            if self.is_free(40, s)? {
                self.set_free(40, s, false)?;
                let p = self.offset(40, s)?;
                self.bytes[p..p + 256].fill(0);
                self.bytes[p + 1] = 255;
                let (t, ls) = *dirs.last().unwrap();
                let prev = self.offset(t, ls)?;
                self.bytes[prev] = 40;
                self.bytes[prev + 1] = s;
                return Ok(p);
            }
        }
        Err("Directory full (296 files maximum).".into())
    }
    fn remove_inner(&mut self, e: &Entry) -> Result<()> {
        for (t, s) in self.chain(e)? {
            self.set_free(t, s, true)?;
        }
        self.bytes[e.offset + 2..e.offset + 32].fill(0);
        Ok(())
    }
    pub fn remove(&mut self, e: &Entry) -> Result<()> {
        self.validate_writable()?;
        if e.flags & 0x40 != 0 {
            return Err("File is locked.".into());
        }
        let mut d = self.clone();
        d.remove_inner(e)?;
        d.validate_writable()?;
        *self = d;
        Ok(())
    }
    pub fn rename(&mut self, e: &Entry, name: &str) -> Result<()> {
        self.validate_writable()?;
        if e.flags & 0x40 != 0 {
            return Err("File is locked.".into());
        }
        let name = encode_name(name, 16)?;
        if let Some(other) = self.find(&name)? {
            if other.offset != e.offset {
                return Err("That filename already exists.".into());
            }
        }
        self.bytes[e.offset + 5..e.offset + 21].fill(0xa0);
        self.bytes[e.offset + 5..e.offset + 5 + name.len()].copy_from_slice(&name);
        Ok(())
    }
    pub fn convert(&self, format: Format) -> Result<Self> {
        if self.format == format {
            // Same-format export preserves every byte, including extended layouts.
            return Ok(self.clone());
        }
        self.validate_writable()?;
        if self.format == Format::D65 {
            for t in 1..=85 {
                let p = self.offset(t, 0)?;
                let start = if t <= 80 { p + 40 * 256 } else { p };
                let end = p + 256 * 256;
                if self.bytes[start..end].iter().any(|b| *b != 0) {
                    return Err(
                        "D65 contains data outside D81 geometry. Conversion would lose data."
                            .into(),
                    );
                }
            }
        }
        let mut d = Self {
            bytes: vec![0; format.size()],
            format,
        };
        for t in 1..=80 {
            let from = self.offset(t, 0)?;
            let to = d.offset(t, 0)?;
            d.bytes[to..to + 40 * 256].copy_from_slice(&self.bytes[from..from + 40 * 256]);
        }
        Ok(d)
    }
}
