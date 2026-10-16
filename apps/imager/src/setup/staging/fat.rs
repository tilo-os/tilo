//! Read-only FAT12/16/32 path lookup, just enough to find the byte ranges of
//! the fixed-size placeholder files the exporter reserves in the staging ESP
//! image (`/EFI/tilo-staging/handoff.json`, `/EFI/tilo-staging/record.json`).
//!
//! The helper writes the handoff mount-free by overwriting those files'
//! data clusters in place: JSON followed by ASCII-space padding to the
//! file's unchanged size. No directory entry, FAT or allocation changes, so
//! no filesystem driver, mount or drive letter is needed on either host, and
//! the image stays verifiable: with the placeholder ranges masked back to
//! spaces its digest equals the manifest digest.

/// A byte range `[offset, offset + len)` inside the filesystem image.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Range {
    pub offset: u64,
    pub len: u64,
}

pub type ReadAt<'a> = dyn FnMut(u64, &mut [u8]) -> Result<(), String> + 'a;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Fat12,
    Fat16,
    Fat32,
}

#[derive(Clone, Copy, Debug)]
struct Volume {
    kind: Kind,
    cluster_bytes: u64,
    fat_offset: u64,
    data_offset: u64,
    clusters: u32,
    /// FAT12/16 fixed root directory.
    root_offset: u64,
    root_bytes: u64,
    /// FAT32 root directory cluster.
    root_cluster: u32,
}

fn le16(b: &[u8], at: usize) -> u64 {
    u64::from(u16::from_le_bytes([b[at], b[at + 1]]))
}

fn le32(b: &[u8], at: usize) -> u64 {
    u64::from(u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]))
}

impl Volume {
    fn parse(read: &mut ReadAt<'_>, image_len: u64) -> Result<Self, String> {
        let mut bpb = [0_u8; 512];
        read(0, &mut bpb)?;
        if bpb[510..512] != [0x55, 0xaa] {
            return Err("staging ESP has no FAT boot sector".into());
        }
        let sector = le16(&bpb, 11);
        let per_cluster = u64::from(bpb[13]);
        let reserved = le16(&bpb, 14);
        let fats = u64::from(bpb[16]);
        let root_entries = le16(&bpb, 17);
        let total = match le16(&bpb, 19) {
            0 => le32(&bpb, 32),
            n => n,
        };
        let fat_sectors = match le16(&bpb, 22) {
            0 => le32(&bpb, 36),
            n => n,
        };
        if !(512..=4096).contains(&sector)
            || !sector.is_power_of_two()
            || per_cluster == 0
            || !per_cluster.is_power_of_two()
            || reserved == 0
            || fats == 0
            || fat_sectors == 0
            || total == 0
        {
            return Err("staging ESP boot sector is not a FAT BPB".into());
        }
        let root_sectors = (root_entries * 32).div_ceil(sector);
        let data_sector = reserved + fats * fat_sectors + root_sectors;
        if data_sector >= total || total * sector > image_len {
            return Err("FAT geometry exceeds the image".into());
        }
        let clusters = (total - data_sector) / per_cluster;
        let kind = if clusters < 4085 {
            Kind::Fat12
        } else if clusters < 65525 {
            Kind::Fat16
        } else {
            Kind::Fat32
        };
        if (kind == Kind::Fat32) != (root_entries == 0) {
            return Err("FAT root directory layout disagrees with its type".into());
        }
        let fat_bytes = fat_sectors * sector;
        let needed = match kind {
            Kind::Fat12 => (clusters + 2) * 3 / 2 + 1,
            Kind::Fat16 => (clusters + 2) * 2,
            Kind::Fat32 => (clusters + 2) * 4,
        };
        if fat_bytes < needed {
            return Err("FAT is too small for its cluster count".into());
        }
        Ok(Self {
            kind,
            cluster_bytes: sector * per_cluster,
            fat_offset: reserved * sector,
            data_offset: data_sector * sector,
            clusters: u32::try_from(clusters).map_err(|_| "too many clusters")?,
            root_offset: (reserved + fats * fat_sectors) * sector,
            root_bytes: root_sectors * sector,
            root_cluster: u32::try_from(le32(&bpb, 44)).unwrap_or(0),
        })
    }

    fn cluster_offset(&self, cluster: u32) -> Result<u64, String> {
        if cluster < 2 || cluster >= self.clusters + 2 {
            return Err(format!("FAT cluster {cluster} out of range"));
        }
        Ok(self.data_offset + u64::from(cluster - 2) * self.cluster_bytes)
    }

    fn next(&self, read: &mut ReadAt<'_>, cluster: u32) -> Result<Option<u32>, String> {
        let n = u64::from(cluster);
        let (value, end, bad) = match self.kind {
            Kind::Fat12 => {
                let mut b = [0_u8; 2];
                read(self.fat_offset + n + n / 2, &mut b)?;
                let raw = u16::from_le_bytes(b);
                let v = if n % 2 == 1 { raw >> 4 } else { raw & 0x0fff };
                (u32::from(v), 0x0ff8, 0x0ff7)
            }
            Kind::Fat16 => {
                let mut b = [0_u8; 2];
                read(self.fat_offset + n * 2, &mut b)?;
                (u32::from(u16::from_le_bytes(b)), 0xfff8, 0xfff7)
            }
            Kind::Fat32 => {
                let mut b = [0_u8; 4];
                read(self.fat_offset + n * 4, &mut b)?;
                (
                    u32::from_le_bytes(b) & 0x0fff_ffff,
                    0x0fff_fff8,
                    0x0fff_fff7,
                )
            }
        };
        if value >= end {
            Ok(None)
        } else if value == bad || value < 2 {
            Err("broken FAT cluster chain".into())
        } else {
            Ok(Some(value))
        }
    }

    /// Clusters of a chain, bounded by the cluster count (no cycles).
    fn chain(&self, read: &mut ReadAt<'_>, first: u32) -> Result<Vec<u32>, String> {
        let mut out = vec![first];
        let mut at = first;
        self.cluster_offset(first)?;
        while let Some(next) = self.next(read, at)? {
            self.cluster_offset(next)?;
            if out.len() > self.clusters as usize {
                return Err("FAT cluster chain loops".into());
            }
            out.push(next);
            at = next;
        }
        Ok(out)
    }

    fn read_dir(&self, read: &mut ReadAt<'_>, cluster: Option<u32>) -> Result<Vec<u8>, String> {
        match cluster {
            None if self.kind != Kind::Fat32 => {
                let mut bytes =
                    vec![0; usize::try_from(self.root_bytes).map_err(|e| e.to_string())?];
                read(self.root_offset, &mut bytes)?;
                Ok(bytes)
            }
            other => {
                let first = other.unwrap_or(self.root_cluster);
                let mut bytes = Vec::new();
                for c in self.chain(read, first)? {
                    let mut chunk = vec![0; self.cluster_bytes as usize];
                    read(self.cluster_offset(c)?, &mut chunk)?;
                    bytes.extend(chunk);
                    if bytes.len() > 4 << 20 {
                        return Err("FAT directory too large".into());
                    }
                }
                Ok(bytes)
            }
        }
    }
}

#[derive(Debug)]
struct Entry {
    name: String,
    directory: bool,
    first_cluster: u32,
    size: u64,
}

fn short_name(raw: &[u8]) -> String {
    let base = String::from_utf8_lossy(&raw[0..8]).trim_end().to_string();
    let ext = String::from_utf8_lossy(&raw[8..11]).trim_end().to_string();
    if ext.is_empty() {
        base
    } else {
        format!("{base}.{ext}")
    }
}

fn entries(dir: &[u8], fat32: bool) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut long: Vec<(u8, Vec<u16>)> = Vec::new();
    for raw in dir.chunks_exact(32) {
        match raw[0] {
            0x00 => break,
            0xe5 => {
                long.clear();
                continue;
            }
            _ => {}
        }
        let attr = raw[11];
        if attr & 0x3f == 0x0f {
            if raw[0] & 0x40 != 0 {
                long.clear();
            }
            let mut units = Vec::with_capacity(13);
            for range in [1..11, 14..26, 28..32] {
                for pair in raw[range].chunks_exact(2) {
                    units.push(u16::from_le_bytes([pair[0], pair[1]]));
                }
            }
            long.push((raw[0] & 0x1f, units));
            continue;
        }
        if attr & 0x08 != 0 {
            long.clear();
            continue;
        }
        let name = if long.is_empty() {
            short_name(raw)
        } else {
            long.sort_by_key(|(seq, _)| *seq);
            let units: Vec<u16> = long
                .iter()
                .flat_map(|(_, u)| u.iter().copied())
                .take_while(|u| *u != 0 && *u != 0xffff)
                .collect();
            String::from_utf16_lossy(&units)
        };
        long.clear();
        // The high cluster word exists only on FAT32 (FAT12/16 reuse it).
        let hi = if fat32 {
            u16::from_le_bytes([raw[20], raw[21]])
        } else {
            0
        };
        out.push(Entry {
            name,
            directory: attr & 0x10 != 0,
            first_cluster: (u32::from(hi) << 16)
                | u32::from(u16::from_le_bytes([raw[26], raw[27]])),
            size: le32(raw, 28),
        });
    }
    out
}

/// Byte ranges holding exactly the contents of the regular file at `path`
/// (`/`-separated, matched case-insensitively against long or short names).
pub fn file_ranges(
    read: &mut ReadAt<'_>,
    image_len: u64,
    path: &str,
) -> Result<Vec<Range>, String> {
    let volume = Volume::parse(read, image_len)?;
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    let (file, dirs) = parts.split_last().ok_or("empty FAT path")?;
    let mut cluster = None;
    for dir in dirs {
        let listing = volume.read_dir(read, cluster)?;
        let entry = entries(&listing, volume.kind == Kind::Fat32)
            .into_iter()
            .find(|e| e.directory && e.name.eq_ignore_ascii_case(dir))
            .ok_or_else(|| format!("staging ESP has no directory {dir:?}"))?;
        cluster = Some(entry.first_cluster);
    }
    let listing = volume.read_dir(read, cluster)?;
    let entry = entries(&listing, volume.kind == Kind::Fat32)
        .into_iter()
        .find(|e| !e.directory && e.name.eq_ignore_ascii_case(file))
        .ok_or_else(|| format!("staging ESP has no placeholder {path}"))?;
    if entry.size == 0 {
        return Err(format!("staging ESP placeholder {path} is empty"));
    }
    let chain = volume.chain(read, entry.first_cluster)?;
    if (chain.len() as u64) < entry.size.div_ceil(volume.cluster_bytes) {
        return Err(format!("staging ESP placeholder {path} is truncated"));
    }
    let mut ranges: Vec<Range> = Vec::new();
    let mut remaining = entry.size;
    for c in chain {
        if remaining == 0 {
            break;
        }
        let len = remaining.min(volume.cluster_bytes);
        let offset = volume.cluster_offset(c)?;
        match ranges.last_mut() {
            Some(last) if last.offset + last.len == offset => last.len += len,
            _ => ranges.push(Range { offset, len }),
        }
        remaining -= len;
    }
    if ranges.iter().any(|r| r.offset + r.len > image_len) {
        return Err("placeholder lies beyond the image".into());
    }
    Ok(ranges)
}

/// Total bytes covered by `ranges`.
pub fn ranges_len(ranges: &[Range]) -> u64 {
    ranges.iter().map(|r| r.len).sum()
}

/// Replace every byte of `chunk` (which starts at image offset `at`) that
/// falls inside `ranges` with an ASCII space.
pub fn mask(chunk: &mut [u8], at: u64, ranges: &[Range]) {
    let end = at + chunk.len() as u64;
    for r in ranges {
        let lo = r.offset.max(at);
        let hi = (r.offset + r.len).min(end);
        if lo < hi {
            chunk[(lo - at) as usize..(hi - at) as usize].fill(b' ');
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A tiny FAT image builder for tests: 512-byte sectors (or `sector`
    /// via [`Builder::with_sector`]), one sector per cluster, one FAT. `fat32` switches to a FAT32 layout with enough
    /// clusters to be classified as FAT32.
    pub(crate) struct Builder {
        pub image: Vec<u8>,
        fat32: bool,
        clusters: u32,
        fat_offset: usize,
        data_offset: usize,
        root_offset: usize,
        next: u32,
        sector: usize,
    }

    impl Builder {
        pub(crate) fn new(fat32: bool) -> Self {
            Self::with_sector(fat32, 512)
        }

        pub(crate) fn with_sector(fat32: bool, sector: usize) -> Self {
            assert!(
                !fat32 || sector == 512,
                "the FAT32 fixture is sized for 512-byte sectors"
            );
            let (total, fat_sectors, root_entries): (u32, u32, u16) = if fat32 {
                (66_000 + 1 + 520, 520, 0)
            } else {
                (128, 1, 16)
            };
            let mut image = vec![0_u8; total as usize * sector];
            image[11..13].copy_from_slice(&(sector as u16).to_le_bytes());
            image[13] = 1;
            image[14..16].copy_from_slice(&1_u16.to_le_bytes());
            image[16] = 1;
            image[17..19].copy_from_slice(&root_entries.to_le_bytes());
            if fat32 {
                image[32..36].copy_from_slice(&total.to_le_bytes());
                image[36..40].copy_from_slice(&fat_sectors.to_le_bytes());
                image[44..48].copy_from_slice(&2_u32.to_le_bytes());
            } else {
                image[19..21].copy_from_slice(&(total as u16).to_le_bytes());
                image[22..24].copy_from_slice(&(fat_sectors as u16).to_le_bytes());
            }
            image[510] = 0x55;
            image[511] = 0xaa;
            let root_sectors = (u32::from(root_entries) * 32).div_ceil(sector as u32);
            let data = 1 + fat_sectors + root_sectors;
            let mut b = Self {
                image,
                fat32,
                clusters: total - data,
                fat_offset: sector,
                data_offset: data as usize * sector,
                root_offset: (1 + fat_sectors) as usize * sector,
                next: 2,
                sector,
            };
            if fat32 {
                b.next = 3;
                b.set_fat(2, 0x0fff_ffff);
            }
            b
        }

        fn set_fat(&mut self, n: u32, value: u32) {
            let n = n as usize;
            if self.fat32 {
                let at = self.fat_offset + n * 4;
                self.image[at..at + 4].copy_from_slice(&value.to_le_bytes());
            } else {
                // FAT12 (128 sectors → < 4085 clusters)
                let at = self.fat_offset + n + n / 2;
                let v = (value & 0x0fff) as u16;
                let old = u16::from_le_bytes([self.image[at], self.image[at + 1]]);
                let new = if n % 2 == 1 {
                    (old & 0x000f) | (v << 4)
                } else {
                    (old & 0xf000) | v
                };
                self.image[at..at + 2].copy_from_slice(&new.to_le_bytes());
            }
        }

        pub(crate) fn cluster_offset(&self, c: u32) -> usize {
            self.data_offset + (c as usize - 2) * self.sector
        }

        /// Allocate `count` clusters as one chain, optionally leaving a hole
        /// after the first to make it fragmented.
        fn alloc(&mut self, count: u32, fragment: bool) -> Vec<u32> {
            let mut out = Vec::new();
            for i in 0..count {
                if fragment && i == 1 {
                    self.next += 1;
                }
                out.push(self.next);
                self.next += 1;
            }
            assert!(self.next < self.clusters + 2);
            for w in out.windows(2) {
                self.set_fat(w[0], w[1]);
            }
            self.set_fat(*out.last().unwrap(), 0x0fff_ffff);
            out
        }

        fn dir_slot(&mut self, dir: Option<u32>) -> usize {
            let base = match dir {
                None if !self.fat32 => self.root_offset,
                None => self.cluster_offset(2),
                Some(c) => self.cluster_offset(c),
            };
            (0..16)
                .map(|i| base + i * 32)
                .find(|&at| self.image[at] == 0)
                .expect("directory slot")
        }

        fn entry(
            &mut self,
            dir: Option<u32>,
            long: Option<&str>,
            short: &[u8; 11],
            attr: u8,
            first: u32,
            size: u32,
        ) {
            if let Some(name) = long {
                let units: Vec<u16> = name.encode_utf16().chain([0]).collect();
                let pieces: Vec<&[u16]> = units.chunks(13).collect();
                for (i, piece) in pieces.iter().enumerate().rev() {
                    let at = self.dir_slot(dir);
                    let mut raw = [0xff_u8; 32];
                    raw[0] = (i as u8 + 1) | if i + 1 == pieces.len() { 0x40 } else { 0 };
                    raw[11] = 0x0f;
                    raw[12] = 0;
                    raw[26] = 0;
                    raw[27] = 0;
                    let slots: Vec<usize> = (1..11)
                        .step_by(2)
                        .chain((14..26).step_by(2))
                        .chain((28..32).step_by(2))
                        .collect();
                    for (j, slot) in slots.iter().enumerate() {
                        if let Some(u) = piece.get(j) {
                            raw[*slot..*slot + 2].copy_from_slice(&u.to_le_bytes());
                        }
                    }
                    self.image[at..at + 32].copy_from_slice(&raw);
                }
            }
            let at = self.dir_slot(dir);
            self.image[at..at + 11].copy_from_slice(short);
            self.image[at + 11] = attr;
            self.image[at + 20..at + 22].copy_from_slice(&((first >> 16) as u16).to_le_bytes());
            self.image[at + 26..at + 28].copy_from_slice(&(first as u16).to_le_bytes());
            self.image[at + 28..at + 32].copy_from_slice(&size.to_le_bytes());
        }

        pub(crate) fn mkdir(
            &mut self,
            parent: Option<u32>,
            long: Option<&str>,
            short: &[u8; 11],
        ) -> u32 {
            let c = self.alloc(1, false)[0];
            self.entry(parent, long, short, 0x10, c, 0);
            c
        }

        pub(crate) fn file(
            &mut self,
            dir: Option<u32>,
            long: Option<&str>,
            short: &[u8; 11],
            content: &[u8],
            fragment: bool,
        ) -> Vec<u32> {
            let chain = self.alloc(
                (content.len() as u32).div_ceil(self.sector as u32).max(1),
                fragment,
            );
            self.entry(dir, long, short, 0x20, chain[0], content.len() as u32);
            for (i, c) in chain.iter().enumerate() {
                let piece = &content[(i * self.sector).min(content.len())
                    ..((i + 1) * self.sector).min(content.len())];
                let at = self.cluster_offset(*c);
                self.image[at..at + piece.len()].copy_from_slice(piece);
            }
            chain
        }
    }

    /// The layout the exporter must produce: `/EFI/tilo-staging/{handoff,record}.json`.
    pub(crate) fn staging_esp_image(fat32: bool, placeholder: usize) -> Vec<u8> {
        staging_esp_image_with_sector(fat32, placeholder, 512)
    }

    pub(crate) fn staging_esp_image_with_sector(
        fat32: bool,
        placeholder: usize,
        sector: usize,
    ) -> Vec<u8> {
        let mut b = Builder::with_sector(fat32, sector);
        let efi = b.mkdir(None, None, b"EFI        ");
        let boot = b.mkdir(Some(efi), None, b"BOOT       ");
        b.file(Some(boot), None, b"BOOTX64 EFI", &[0x4d; 700], false);
        let state = b.mkdir(Some(efi), Some("tilo-staging"), b"TILO-S~1   ");
        let pad = vec![b' '; placeholder];
        b.file(
            Some(state),
            Some("handoff.json"),
            b"HANDOF~1JSO",
            &pad,
            true,
        );
        b.file(
            Some(state),
            Some("record.json"),
            b"RECORD~1JSO",
            &pad,
            false,
        );
        b.image
    }

    fn reader(image: &[u8]) -> impl FnMut(u64, &mut [u8]) -> Result<(), String> + '_ {
        move |at, buf| {
            let at = at as usize;
            let src = image.get(at..at + buf.len()).ok_or("read past end")?;
            buf.copy_from_slice(src);
            Ok(())
        }
    }

    /// FAT12/FAT32 placeholders, corrupt structures and masking.
    #[test]
    fn fat_placeholders_are_found_and_masked_precisely() {
        // Phase `finds_fragmented_long_named_placeholders_on_fat12`.
        {
            let image = staging_esp_image(false, 1500);
            let ranges = file_ranges(
                &mut reader(&image),
                image.len() as u64,
                "/EFI/tilo-staging/handoff.json",
            )
            .unwrap();
            assert_eq!(ranges_len(&ranges), 1500);
            assert_eq!(
                ranges.len(),
                2,
                "one hole after the first cluster: {ranges:?}"
            );
            for r in &ranges {
                assert!(
                    image[r.offset as usize..(r.offset + r.len) as usize]
                        .iter()
                        .all(|b| *b == b' ')
                );
            }
            let record = file_ranges(
                &mut reader(&image),
                image.len() as u64,
                "/efi/TILO-STAGING/Record.JSON",
            )
            .unwrap();
            assert_eq!(record.len(), 1);
            assert_eq!(ranges_len(&record), 1500);
            let loader = file_ranges(
                &mut reader(&image),
                image.len() as u64,
                "/EFI/BOOT/BOOTX64.EFI",
            )
            .unwrap();
            assert_eq!(ranges_len(&loader), 700);
        }

        // Phase `finds_placeholders_on_fat32`.
        {
            let image = staging_esp_image(true, 4096);
            let ranges = file_ranges(
                &mut reader(&image),
                image.len() as u64,
                "/EFI/tilo-staging/handoff.json",
            )
            .unwrap();
            assert_eq!(ranges_len(&ranges), 4096);
        }

        // Phase `missing_or_corrupt_structures_are_errors`.
        {
            let image = staging_esp_image(false, 1024);
            let len = image.len() as u64;
            assert!(file_ranges(&mut reader(&image), len, "/EFI/tilo-staging/other.json").is_err());
            assert!(file_ranges(&mut reader(&image), len, "/EFI/nothing/handoff.json").is_err());
            let mut no_sig = image.clone();
            no_sig[510] = 0;
            assert!(
                file_ranges(&mut reader(&no_sig), len, "/EFI/tilo-staging/handoff.json").is_err()
            );
            assert!(
                file_ranges(
                    &mut reader(&image),
                    len / 2,
                    "/EFI/tilo-staging/handoff.json"
                )
                .is_err()
            );
            // A FAT chain that loops back on itself.
            let mut looped = Builder::new(false);
            let chain = looped.file(None, None, b"LOOP    BIN", &[b' '; 2048], false);
            looped.set_fat(chain[3], chain[0]);
            let l = looped.image.len() as u64;
            assert!(file_ranges(&mut reader(&looped.image), l, "/LOOP.BIN").is_err());
        }

        // Phase `masking_restores_spaces_only_inside_ranges`.
        {
            let ranges = [
                Range { offset: 10, len: 5 },
                Range {
                    offset: 30,
                    len: 100,
                },
            ];
            let mut chunk = vec![b'x'; 20];
            mask(&mut chunk, 8, &ranges);
            assert_eq!(&chunk, b"xx     xxxxxxxxxxxxx");
            let mut tail = vec![b'x'; 4];
            mask(&mut tail, 28, &ranges);
            assert_eq!(&tail, b"xx  ");
        }
    }
}
