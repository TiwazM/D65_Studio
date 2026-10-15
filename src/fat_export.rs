//! New-file-only FAT32 exporter. Operates on an exclusively held, unmounted
//! partition, never on a mounted filesystem. No formatting or defragmentation.
use crate::disk::{Disk, Result};
use std::collections::{BTreeMap, VecDeque};

pub type Sector = [u8; 512];
pub const TRANSFER_BYTES: usize = 256 * 1024;
#[derive(Clone, Copy)]
pub enum Phase {
    Allocation = 1,
    Directories,
    Journal,
    Writing,
    Verifying,
    Publishing,
    Recovering,
}
pub trait Device {
    fn sectors(&self) -> u64;
    fn read(&mut self, sector: u64) -> Result<Sector>;
    fn write(&mut self, sector: u64, data: &Sector) -> Result<()>;
    fn sync(&mut self) -> Result<()>;
    // Portable fallbacks preserve existing Mac/device implementations and the
    // sector-level fault-injection tests. Native adapters can batch actual I/O.
    fn read_sectors(&mut self, sector: u64, bytes: &mut [u8]) -> Result<()> {
        ensure(bytes.len() % 512 == 0, "Unaligned transfer size")?;
        for (i, chunk) in bytes.chunks_exact_mut(512).enumerate() {
            chunk.copy_from_slice(&self.read(sector + i as u64)?);
        }
        Ok(())
    }
    fn write_sectors(&mut self, sector: u64, bytes: &[u8]) -> Result<()> {
        ensure(bytes.len() % 512 == 0, "Unaligned transfer size")?;
        for (i, chunk) in bytes.chunks_exact(512).enumerate() {
            self.write(sector + i as u64, chunk.try_into().unwrap())?;
        }
        Ok(())
    }
    fn progress(&mut self, _phase: Phase, _done: u64, _total: u64) {}
}
#[derive(Clone)]
pub struct Patch {
    pub sector: u64,
    pub before: Sector,
    pub after: Sector,
}
#[derive(Debug)]
pub struct Receipt {
    pub first_cluster: u32,
    pub clusters: u32,
    pub bytes: usize,
}
fn u16at(b: &[u8], p: usize) -> u16 {
    u16::from_le_bytes(b[p..p + 2].try_into().unwrap())
}
fn u32at(b: &[u8], p: usize) -> u32 {
    u32::from_le_bytes(b[p..p + 4].try_into().unwrap())
}
fn set32(b: &mut [u8], p: usize, n: u32) {
    b[p..p + 4].copy_from_slice(&n.to_le_bytes());
}
fn ensure(ok: bool, error: &str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(error.into())
    }
}

struct Volume {
    reserved: u32,
    fat_sectors: u32,
    data: u32,
    clusters: u32,
    root: u32,
    fat: Vec<u8>,
    infos: Vec<u64>,
}
impl Volume {
    fn load(d: &mut impl Device) -> Result<Self> {
        let b = d.read(0)?;
        ensure(b[510..] == [0x55, 0xaa] && u16at(&b,11)==512 && b[13]==8,
            "This exporter requires a FAT32 partition with 512-byte sectors and 4096-byte clusters.")?;
        ensure(
            b[16] == 2
                && u16at(&b, 17) == 0
                && u16at(&b, 19) == 0
                && u16at(&b, 22) == 0
                && u16at(&b, 40) == 0
                && u16at(&b, 42) == 0,
            "Unsupported FAT32 geometry or disabled FAT mirroring.",
        )?;
        let reserved = u16at(&b, 14) as u32;
        let fat_sectors = u32at(&b, 36);
        let total = u32at(&b, 32);
        let data = (reserved as u64) + 2 * (fat_sectors as u64);
        ensure(
            reserved >= 2
                && fat_sectors > 0
                && fat_sectors <= 131072
                && data < total as u64
                && total as u64 <= d.sectors(),
            "Invalid or unsupported FAT32 partition bounds.",
        )?;
        let clusters = (total - data as u32) / 8;
        let root = u32at(&b, 44);
        ensure(
            (65525..0x0ffffff5).contains(&clusters)
                && (clusters as u64 + 2) * 4 <= fat_sectors as u64 * 512
                && root >= 2
                && root < clusters + 2,
            "Invalid FAT32 cluster count or root directory.",
        )?;
        let mut fat = vec![0; fat_sectors as usize * 512];
        let mut mirror = vec![0; TRANSFER_BYTES];
        let total = fat.len() as u64;
        for (n, a) in fat.chunks_mut(TRANSFER_BYTES).enumerate() {
            let offset = (n * TRANSFER_BYTES / 512) as u64;
            d.progress(Phase::Allocation, offset * 512, total);
            d.read_sectors(reserved as u64 + offset, a)?;
            d.read_sectors(
                (reserved + fat_sectors) as u64 + offset,
                &mut mirror[..a.len()],
            )?;
            ensure(
                a == &mirror[..a.len()],
                "The two FAT copies differ; repair the card before exporting.",
            )?;
        }
        ensure(
            u32at(&fat, 0) & 0x0fffffff == 0x0fffff00 | b[21] as u32,
            "Invalid FAT media entry.",
        )?;
        ensure(u32at(&fat,4)&0x0fffffff==0x0fffffff,
            "The FAT32 volume is marked dirty or has recorded an I/O error. Repair it before exporting.")?;
        let info = u16at(&b, 48) as u32;
        let backup = u16at(&b, 50) as u32;
        let mut infos = Vec::new();
        if info != 0xffff && info != 0 {
            ensure(info < reserved, "FSInfo lies outside the reserved area.")?;
            infos.push(info as u64);
        }
        if backup != 0xffff && backup != 0 {
            ensure(
                backup < reserved && d.read(backup as u64)? == b,
                "Invalid or mismatched backup boot sector.",
            )?;
            if !infos.is_empty() {
                ensure(
                    backup + info < reserved,
                    "Backup FSInfo lies outside the reserved area.",
                )?;
                infos.push((backup + info) as u64);
            }
        }
        let mut valid_infos = Vec::new();
        for &s in &infos {
            let b = d.read(s)?;
            // FSInfo is only a free-space/search hint. We count/allocate from
            // both audited FATs, never these counters. Leave unrecognized
            // optional sectors untouched instead of treating them as damage.
            if u32at(&b, 0) == 0x41615252
                && u32at(&b, 484) == 0x61417272
                && u32at(&b, 508) == 0xaa550000
            {
                valid_infos.push(s);
            }
        }
        Ok(Self {
            reserved,
            fat_sectors,
            data: data as u32,
            clusters,
            root,
            fat,
            infos: valid_infos,
        })
    }
    fn next(&self, c: u32) -> u32 {
        u32at(&self.fat, c as usize * 4) & 0x0fffffff
    }
    fn sector(&self, c: u32) -> u64 {
        self.data as u64 + (c as u64 - 2) * 8
    }
    fn chain(&self, start: u32, used: &mut [bool]) -> Result<Vec<u32>> {
        let mut c = start;
        let mut result = Vec::new();
        loop {
            ensure(
                c >= 2 && c < self.clusters + 2,
                "A file references an invalid cluster.",
            )?;
            ensure(
                !used[c as usize],
                "Cross-linked files or a cyclic FAT chain; repair the card first.",
            )?;
            used[c as usize] = true;
            result.push(c);
            let next = self.next(c);
            if next >= 0x0ffffff8 {
                break;
            }
            ensure(
                next >= 2 && next < self.clusters + 2,
                "A file references a free, bad or reserved cluster.",
            )?;
            c = next;
        }
        Ok(result)
    }
    // Audit directory-reachable chains before trusting free entries. This prevents
    // an inconsistent 'free' cluster from hiding data referenced by another file.
    fn audit(
        &self,
        d: &mut impl Device,
        name: &[u8; 11],
        target: u32,
    ) -> Result<(Vec<u64>, usize)> {
        let mut used = vec![false; self.clusters as usize + 2];
        let mut pending = VecDeque::from([(self.root, self.root == target)]);
        let mut target_sectors = Vec::new();
        let mut target_end = None;
        let mut directory_count = 0;
        while let Some((start, is_target)) = pending.pop_front() {
            directory_count += 1;
            d.progress(Phase::Directories, directory_count, 0);
            ensure(
                directory_count <= 100000,
                "Directory tree exceeds the supported inspection limit.",
            )?;
            let chain = self.chain(start, &mut used)?;
            let sectors: Vec<_> = chain
                .iter()
                .flat_map(|&c| self.sector(c)..self.sector(c) + 8)
                .collect();
            if is_target {
                target_sectors = sectors.clone();
            }
            let mut end = false;
            let mut lfn: Vec<[u16; 13]> = Vec::new();
            let mut cluster_buffer = [0; 4096];
            for (si, s) in sectors.iter().enumerate() {
                if end {
                    break;
                }
                if si % 8 == 0 {
                    d.read_sectors(*s, &mut cluster_buffer)?;
                }
                let b = &cluster_buffer[si % 8 * 512..(si % 8 + 1) * 512];
                for (i, e) in b.chunks_exact(32).enumerate() {
                    if e[0] == 0 {
                        ensure(
                            lfn.is_empty(),
                            "Unfinished long filename at the directory end; repair the card first.",
                        )?;
                        if is_target {
                            target_end = Some(si * 16 + i);
                        }
                        end = true;
                        break;
                    }
                    if e[0] == 0xe5 {
                        lfn.clear();
                        continue;
                    }
                    if e[11] == 0x0f {
                        ensure(lfn.len() < 20, "Malformed long filename sequence.")?;
                        let mut chars = [0u16; 13];
                        for (j, p) in [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30]
                            .iter()
                            .enumerate()
                        {
                            chars[j] = u16at(e, *p);
                        }
                        lfn.push(chars);
                        continue;
                    }
                    if is_target {
                        ensure(
                            !e[..11].eq_ignore_ascii_case(name),
                            "That filename already exists on the card. Choose a new name.",
                        )?;
                        let long: Vec<_> = lfn
                            .iter()
                            .rev()
                            .flatten()
                            .copied()
                            .take_while(|&c| c != 0 && c != 0xffff)
                            .collect();
                        let long = String::from_utf16_lossy(&long);
                        let target = format!(
                            "{}.{}",
                            String::from_utf8_lossy(&name[..8]).trim_end(),
                            String::from_utf8_lossy(&name[8..])
                        );
                        ensure(
                            !long.eq_ignore_ascii_case(&target),
                            "That long filename already exists on the card. Choose a new name.",
                        )?;
                    }
                    lfn.clear();
                    if e[11] & 8 != 0 {
                        continue;
                    }
                    if &e[..11] == b".          " || &e[..11] == b"..         " {
                        continue;
                    }
                    let first = ((u16at(e, 20) as u32) << 16) | u16at(e, 26) as u32;
                    let size = u32at(e, 28) as u64;
                    if e[11] & 0x10 != 0 {
                        pending.push_back((first, first == target));
                    } else if first != 0 {
                        let chain = self.chain(first, &mut used)?;
                        ensure(
                            chain.len() as u64 * 4096 >= size,
                            "A file's cluster chain is shorter than its size.",
                        )?;
                    } else {
                        ensure(size == 0, "A nonempty file has no allocated cluster chain.")?;
                    }
                }
            }
        }
        let end=target_end.ok_or("The destination directory has no unused end slot. Choose another existing folder; this exporter does not expand directories.")?;
        Ok((target_sectors, end))
    }
    fn directory(&self, d: &mut impl Device, parts: &[String]) -> Result<u32> {
        ensure(parts.len() <= 64, "Destination folder path is too deep")?;
        let mut current = self.root;
        let mut visited = vec![false; self.clusters as usize + 2];
        for part in parts {
            ensure(
                !part.is_empty()
                    && part != "."
                    && part != ".."
                    && !part
                        .chars()
                        .any(|c| c.is_control() || "/\\:*?\"<>|".contains(c))
                    && !part.ends_with(['.', ' ']),
                "Invalid destination folder component",
            )?;
            let mut found = None;
            let mut lfn = Vec::<[u8; 32]>::new();
            let chain = self.chain(current, &mut visited)?;
            'directory: for cluster in chain {
                let mut buffer = [0; 4096];
                d.read_sectors(self.sector(cluster), &mut buffer)?;
                for entry in buffer.chunks_exact(32) {
                    if entry[0] == 0 {
                        break 'directory;
                    }
                    if entry[0] == 0xe5 {
                        lfn.clear();
                        continue;
                    }
                    if entry[11] == 0x0f {
                        ensure(lfn.len() < 20, "Malformed long filename sequence")?;
                        lfn.push(entry.try_into().unwrap());
                        continue;
                    }
                    let short = short_display(&entry[..11]);
                    let long = long_name(&lfn, &entry[..11]);
                    let matched = short.eq_ignore_ascii_case(part)
                        || long.as_ref().is_some_and(|n| n.eq_ignore_ascii_case(part));
                    lfn.clear();
                    if !matched {
                        continue;
                    }
                    ensure(
                        entry[11] & 0x18 == 0x10,
                        "Destination component is not a directory",
                    )?;
                    ensure(found.is_none(), "Ambiguous destination directory name")?;
                    found = Some(((u16at(entry, 20) as u32) << 16) | u16at(entry, 26) as u32);
                }
            }
            current = found.ok_or_else(|| format!("Destination folder '{part}' was not found in the audited FAT32 directory. Use an existing folder or its short alias."))?;
        }
        Ok(current)
    }
}

fn short_display(name: &[u8]) -> String {
    let stem = String::from_utf8_lossy(&name[..8]);
    let ext = String::from_utf8_lossy(&name[8..]);
    if ext.trim_end().is_empty() {
        stem.trim_end().into()
    } else {
        format!("{}.{}", stem.trim_end(), ext.trim_end())
    }
}
fn long_name(entries: &[[u8; 32]], short: &[u8]) -> Option<String> {
    if entries.is_empty() {
        return None;
    }
    let checksum = short
        .iter()
        .fold(0u8, |sum, b| sum.rotate_right(1).wrapping_add(*b));
    let count = entries.len();
    let mut chars = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        let ordinal = (count - i) as u8 | if i == 0 { 0x40 } else { 0 };
        if entry[0] != ordinal
            || entry[11] != 0x0f
            || entry[12] != 0
            || entry[13] != checksum
            || u16at(entry, 26) != 0
        {
            return None;
        }
    }
    for entry in entries.iter().rev() {
        for p in [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30] {
            chars.push(u16at(entry, p));
        }
    }
    let end = chars.iter().position(|c| *c == 0).unwrap_or(chars.len());
    if chars[..end].contains(&0xffff)
        || (end < chars.len() && chars[end + 1..].iter().any(|c| *c != 0xffff))
    {
        return None;
    }
    String::from_utf16(&chars[..end]).ok()
}

pub fn short_name(name: &str) -> Result<[u8; 11]> {
    let (stem, ext) = name
        .rsplit_once('.')
        .ok_or("Use a new uppercase 8.3 image filename.")?;
    ensure(
        !stem.is_empty()
            && stem.len() <= 8
            && matches!(ext, "D65" | "D81")
            && stem
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_' || b == b'-'),
        "Use 1–8 uppercase letters, digits, - or _, followed by .D65 or .D81.",
    )?;
    let mut out = [b' '; 11];
    out[..stem.len()].copy_from_slice(stem.as_bytes());
    out[8..].copy_from_slice(ext.as_bytes());
    Ok(out)
}
fn patch<'a>(
    d: &mut impl Device,
    p: &'a mut BTreeMap<u64, Patch>,
    s: u64,
) -> Result<&'a mut Sector> {
    if !p.contains_key(&s) {
        let before = d.read(s)?;
        p.insert(
            s,
            Patch {
                sector: s,
                before,
                after: before,
            },
        );
    }
    Ok(&mut p.get_mut(&s).unwrap().after)
}

/// The caller durably records original metadata before the first write. Both
/// allocation tables are updated, data is flushed/checked before publication,
/// and caught failures restore original metadata. Power-loss is not atomic:
/// the dirty bit and journal intentionally remain available for recovery.
pub fn export(
    d: &mut impl Device,
    name: &str,
    disk: &Disk,
    save_journal: impl FnOnce(&[Patch]) -> Result<()>,
) -> Result<Receipt> {
    export_to(d, &[], name, disk, save_journal)
}

/// Export into an existing root/subdirectory, resolving names while raw and
/// exclusively locked. Never creates folders or trusts a pre-lock cluster ID.
pub fn export_to(
    d: &mut impl Device,
    folders: &[String],
    name: &str,
    disk: &Disk,
    save_journal: impl FnOnce(&[Patch]) -> Result<()>,
) -> Result<Receipt> {
    let name11 = short_name(name)?;
    ensure(
        name.ends_with(&disk.format.ext().to_ascii_uppercase())
            && disk.bytes.len() == disk.format.size(),
        "Image type/length does not match its filename.",
    )?;
    let v = Volume::load(d)?;
    let target = v.directory(d, folders)?;
    let (dirs, slot) = v.audit(d, &name11, target)?;
    let count = disk.bytes.len().div_ceil(4096) as u32;
    let mut run = 0;
    let mut first = None;
    for c in 2..v.clusters + 2 {
        if v.next(c) == 0 {
            run += 1;
        } else {
            run = 0;
        }
        if run == count {
            first = Some(c + 1 - count);
            break;
        }
    }
    let first = first
        .ok_or("No sufficiently long run of free clusters. Existing files were not changed.")?;
    let free_after = (2..v.clusters + 2).filter(|&c| v.next(c) == 0).count() as u32 - count;
    let next_free = (first + count..v.clusters + 2)
        .chain(2..first)
        .find(|&c| v.next(c) == 0)
        .unwrap_or(u32::MAX);
    let mut patches = BTreeMap::new();
    for copy in 0..2 {
        let base = (v.reserved + copy * v.fat_sectors) as u64;
        patch(d, &mut patches, base)?; // journal the dirty flag even if allocation starts later
        for c in first..first + count {
            let s = base + (c / 128) as u64;
            let p = (c % 128) as usize * 4;
            let b = patch(d, &mut patches, s)?;
            let next = if c + 1 == first + count {
                0x0fffffff
            } else {
                c + 1
            };
            let value = (u32at(b, p) & 0xf0000000) | next;
            set32(b, p, value);
        }
    }
    for &s in &v.infos {
        let b = patch(d, &mut patches, s)?;
        set32(b, 488, free_after);
        set32(b, 492, next_free);
    }
    let dir_sector = dirs[slot / 16];
    let offset = slot % 16 * 32;
    let b = patch(d, &mut patches, dir_sector)?;
    b[offset..offset + 32].fill(0);
    b[offset..offset + 11].copy_from_slice(&name11);
    b[offset + 11] = 0x20;
    // A valid fixed FAT date (1980-01-01) rather than an invalid zero date.
    b[offset + 16..offset + 18].copy_from_slice(&33u16.to_le_bytes());
    b[offset + 24..offset + 26].copy_from_slice(&33u16.to_le_bytes());
    b[offset + 20..offset + 22].copy_from_slice(&((first >> 16) as u16).to_le_bytes());
    b[offset + 26..offset + 28].copy_from_slice(&(first as u16).to_le_bytes());
    set32(b, offset + 28, disk.bytes.len() as u32);
    if slot + 1 < dirs.len() * 16 {
        patch(d, &mut patches, dirs[(slot + 1) / 16])?[(slot + 1) % 16 * 32] = 0;
    }
    let patches: Vec<_> = patches.into_values().collect();
    d.progress(Phase::Journal, 0, 0);
    save_journal(&patches)?;
    // Recheck snapshots after journalling: do not write if anything changed.
    for p in &patches {
        ensure(
            d.read(p.sector)? == p.before,
            "Card metadata changed during export preparation; no writes performed.",
        )?;
    }
    let headers = [v.reserved as u64, (v.reserved + v.fat_sectors) as u64];
    let dirty = |mut b: Sector| {
        let n = u32at(&b, 4) & !0x08000000;
        set32(&mut b, 4, n);
        b
    };
    let result: Result<Receipt> = (|| {
        for &s in &headers {
            let b = d.read(s)?;
            d.write(s, &dirty(b))?;
        }
        d.sync()?;
        // Data goes only into clusters proven free by both FATs and directory audit.
        let padded = count as usize * 4096;
        let mut buffer = vec![0; TRANSFER_BYTES];
        for from in (0..padded).step_by(TRANSFER_BYTES) {
            let length = (padded - from).min(TRANSFER_BYTES);
            buffer[..length].fill(0);
            let to = (from + length).min(disk.bytes.len());
            if from < to {
                buffer[..to - from].copy_from_slice(&disk.bytes[from..to]);
            }
            d.progress(Phase::Writing, from as u64, padded as u64);
            d.write_sectors(v.sector(first) + (from / 512) as u64, &buffer[..length])?;
        }
        d.sync()?;
        for (n, chunk) in disk.bytes.chunks(TRANSFER_BYTES).enumerate() {
            d.progress(
                Phase::Verifying,
                (n * TRANSFER_BYTES) as u64,
                disk.bytes.len() as u64,
            );
            d.read_sectors(
                v.sector(first) + (n * TRANSFER_BYTES / 512) as u64,
                &mut buffer[..chunk.len()],
            )?;
            ensure(
                &buffer[..chunk.len()] == chunk,
                "Raw data read-back failed.",
            )?;
        }
        d.progress(Phase::Publishing, 0, 0);
        // Publish directory entries only after allocation tables and data are durable.
        for p in patches.iter().filter(|p| !dirs.contains(&p.sector)) {
            d.write(
                p.sector,
                &if headers.contains(&p.sector) {
                    dirty(p.after)
                } else {
                    p.after
                },
            )?;
        }
        d.sync()?;
        for p in patches.iter().filter(|p| dirs.contains(&p.sector)) {
            d.write(p.sector, &p.after)?;
        }
        d.sync()?;
        for p in &patches {
            let expected = if headers.contains(&p.sector) {
                dirty(p.after)
            } else {
                p.after
            };
            ensure(
                d.read(p.sector)? == expected,
                "FAT/directory read-back failed.",
            )?;
        }
        // Expected FAT entries are c+1 and EOC; comparison above verifies both copies.
        for &s in &headers {
            d.write(s, &patches.iter().find(|p| p.sector == s).unwrap().after)?;
        }
        d.sync()?;
        for &s in &headers {
            ensure(
                d.read(s)? == patches.iter().find(|p| p.sector == s).unwrap().after,
                "Final clean-flag verification failed.",
            )?;
        }
        Ok(Receipt {
            first_cluster: first,
            clusters: count,
            bytes: disk.bytes.len(),
        })
    })();
    match result {
        Ok(r) => Ok(r),
        Err(error) => {
            d.progress(Phase::Recovering, 0, 0);
            let rollback = (|| {
                // Remove a possibly published new entry first, restore other metadata,
                // and clear dirty flags only after the restored state is verified.
                for p in patches.iter().filter(|p| dirs.contains(&p.sector)) {
                    d.write(p.sector, &p.before)?;
                }
                d.sync()?;
                for p in patches.iter().filter(|p| !dirs.contains(&p.sector)) {
                    d.write(
                        p.sector,
                        &if headers.contains(&p.sector) {
                            dirty(p.before)
                        } else {
                            p.before
                        },
                    )?;
                }
                d.sync()?;
                for p in &patches {
                    ensure(
                        d.read(p.sector)?
                            == if headers.contains(&p.sector) {
                                dirty(p.before)
                            } else {
                                p.before
                            },
                        "Rollback verification failed.",
                    )?;
                }
                for &s in &headers {
                    d.write(s, &patches.iter().find(|p| p.sector == s).unwrap().before)?;
                }
                d.sync()?;
                for &s in &headers {
                    ensure(
                        d.read(s)? == patches.iter().find(|p| p.sector == s).unwrap().before,
                        "Final rollback verification failed.",
                    )?;
                }
                Ok::<(), String>(())
            })();
            match rollback {
                Ok(()) => Err(format!(
                    "{error} Original filesystem metadata restored; export cancelled."
                )),
                Err(e) => {
                    for &s in &headers {
                        if let Ok(b) = d.read(s) {
                            let _ = d.write(s, &dirty(b));
                        }
                    }
                    let _ = d.sync();
                    Err(format!("RECOVERY REQUIRED: {error}; metadata restore also failed: {e}. Keep the card unmounted and retain the journal."))
                }
            }
        }
    }
}
