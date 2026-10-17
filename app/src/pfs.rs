//! PlayStation File System (PFS) reader, including PFSC compressed files.
//!
//! Three inode layouts exist (see HANDOFF.md section 5):
//!
//! | mode      | size  | layout                                    |
//! |-----------|-------|-------------------------------------------|
//! | unsigned  | 0xA8  | 100-byte header + 17 x u32 pointers       |
//! | signed-32 | 0x2C8 | 100-byte header + 17 x (32-byte sig + u32)|
//! | signed-64 | 0x310 | 104-byte header + 17 x (32-byte sig + u64)|

use std::sync::{Arc, Mutex};

use flate2::{Decompress, FlushDecompress, Status};

use crate::binary::{check_bounds, i32le, i64le, u16le, u32le, u64le, SharedSource, Source};
use crate::crypto::{pfs_xts_keys, XtsSource, XTS_SECTOR_SIZE};
use crate::error::{Error, Result};

pub const PFS_MAGIC: u64 = 20130315;
const HEADER_SIZE: usize = 0x380;
const SEED_OFFSET: usize = 0x370;

const MODE_SIGNED: u16 = 0x1;
const MODE_64BIT: u16 = 0x2;
const MODE_ENCRYPTED: u16 = 0x4;

const INODE_MODE_DIR: u16 = 0x4000;
const INODE_MODE_FILE: u16 = 0x8000;
const INODE_FLAG_COMPRESSED: u32 = 0x1;

const DIRENT_FILE: i32 = 2;
const DIRENT_DIR: i32 = 3;
const DIRENT_DOT: i32 = 4;
const DIRENT_DOTDOT: i32 = 5;

const PFSC_MAGIC: &[u8; 4] = b"PFSC";
const PFSC_HEADER_SIZE: u64 = 0x30;
const PFSC_MAX_BLOCK: u64 = 0x100000;

const MAX_INODE_BLOCKS: u64 = 1 << 20;
const MAX_BLOCK_INDEX: u64 = 1 << 26;

fn corrupt<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Corrupt(msg.into()))
}

#[derive(Clone, Debug)]
pub struct PfsHeader {
    pub version: u64,
    pub mode: u16,
    pub block_size: u64,
    pub nblock: u64,
    pub inode_count: u64,
    pub inode_block_count: u64,
    pub superroot_inode: u64,
    pub seed: [u8; 16],
}

impl PfsHeader {
    pub fn parse(d: &[u8]) -> Result<Self> {
        if d.len() < HEADER_SIZE {
            return Err(Error::Truncated(format!("PFS superblock needs 0x{HEADER_SIZE:X} bytes")));
        }
        let version = u64le(d, 0x00);
        let magic = u64le(d, 0x08);
        if magic != PFS_MAGIC {
            return corrupt(format!(
                "PFS superblock magic is {magic} (expected {PFS_MAGIC}); \
                 the image is not a plaintext PFS or the key is wrong"
            ));
        }
        if version != 1 && version != 2 {
            return corrupt(format!("unsupported PFS version {version}"));
        }
        Ok(PfsHeader {
            version,
            mode: u16le(d, 0x1C),
            block_size: u32le(d, 0x20) as u64,
            nblock: u64le(d, 0x28),
            inode_count: u64le(d, 0x30),
            inode_block_count: u64le(d, 0x40),
            superroot_inode: u64le(d, 0x48),
            seed: d[SEED_OFFSET..SEED_OFFSET + 16].try_into().unwrap(),
        })
    }

    pub fn signed(&self) -> bool {
        self.mode & MODE_SIGNED != 0
    }
    pub fn is_64bit(&self) -> bool {
        self.mode & MODE_64BIT != 0
    }
    pub fn encrypted(&self) -> bool {
        self.mode & MODE_ENCRYPTED != 0
    }
    pub fn inode_size(&self) -> u64 {
        match (self.signed(), self.is_64bit()) {
            (false, _) => 0xA8,
            (true, true) => 0x310,
            (true, false) => 0x2C8,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Inode {
    pub number: u64,
    pub mode: u16,
    pub flags: u32,
    pub size: i64,
    pub size_compressed: i64,
    pub blocks: u32,
    pub direct: [u64; 12],
    pub indirect: [u64; 5],
}

impl Inode {
    fn parse(d: &[u8], number: u64, h: &PfsHeader) -> Result<Self> {
        if (d.len() as u64) < h.inode_size() {
            return corrupt(format!("inode {number} record is short"));
        }
        let mut direct = [0u64; 12];
        let mut indirect = [0u64; 5];
        if !h.signed() {
            for i in 0..12 {
                direct[i] = u32le(d, 0x64 + i * 4) as u64;
            }
            for i in 0..5 {
                indirect[i] = u32le(d, 0x94 + i * 4) as u64;
            }
        } else if h.is_64bit() {
            let base = 0x68;
            for i in 0..12 {
                direct[i] = u64le(d, base + i * 40 + 32);
            }
            for i in 0..5 {
                indirect[i] = u64le(d, base + 12 * 40 + i * 40 + 32);
            }
        } else {
            let base = 0x64;
            for i in 0..12 {
                direct[i] = u32le(d, base + i * 36 + 32) as u64;
            }
            for i in 0..5 {
                indirect[i] = u32le(d, base + 12 * 36 + i * 36 + 32) as u64;
            }
        }
        Ok(Inode {
            number,
            mode: u16le(d, 0x00),
            flags: u32le(d, 0x04),
            size: i64le(d, 0x08),
            size_compressed: i64le(d, 0x10),
            blocks: u32le(d, 0x60),
            direct,
            indirect,
        })
    }

    pub fn is_dir(&self) -> bool {
        self.mode & INODE_MODE_DIR != 0
    }
    pub fn is_file(&self) -> bool {
        self.mode & INODE_MODE_FILE != 0
    }
    pub fn is_compressed(&self) -> bool {
        self.flags & INODE_FLAG_COMPRESSED != 0
            || (self.size_compressed != 0 && self.size_compressed != self.size)
    }
    /// File size; a negative on-disk size reads as an empty file.
    pub fn len(&self) -> u64 {
        self.size.max(0) as u64
    }
}

#[derive(Clone, Debug)]
pub struct Dirent {
    pub inode_number: u64,
    pub type_code: i32,
    pub name: String,
}

impl Dirent {
    pub fn is_dir(&self) -> bool {
        self.type_code == DIRENT_DIR
    }
    pub fn is_file(&self) -> bool {
        self.type_code == DIRENT_FILE
    }
}

/// Physical block numbers of an inode. Contiguous runs are kept as a range so
/// a huge file does not allocate one entry per block.
#[derive(Clone, Debug)]
pub enum BlockMap {
    Run { start: u64, count: u64 },
    List(Vec<u64>),
}

impl BlockMap {
    pub fn len(&self) -> u64 {
        match self {
            BlockMap::Run { count, .. } => *count,
            BlockMap::List(v) => v.len() as u64,
        }
    }
    pub fn get(&self, index: u64) -> Option<u64> {
        match self {
            BlockMap::Run { start, count } => (index < *count).then(|| start + index),
            BlockMap::List(v) => v.get(index as usize).copied(),
        }
    }
}

pub struct PfsImage {
    reader: SharedSource,
    pub header: PfsHeader,
    pub inodes: Vec<Inode>,
    root: u64,
}

impl PfsImage {
    pub fn open(source: SharedSource, ekpfs: Option<&[u8; 32]>, new_crypt: bool) -> Result<Arc<Self>> {
        let header = PfsHeader::parse(&source.read_at(0, HEADER_SIZE)?)?;
        validate_header(&header)?;

        let reader: SharedSource = if header.encrypted() {
            let Some(key) = ekpfs else {
                return Err(Error::KeyRequired(
                    "PFS image is encrypted (mode bit 0x4) and no EKPFS key was supplied".into(),
                ));
            };
            let (data_key, tweak_key) = pfs_xts_keys(key, &header.seed, new_crypt);
            Arc::new(XtsSource::new(
                source,
                data_key,
                tweak_key,
                header.block_size / XTS_SECTOR_SIZE,
            ))
        } else {
            source
        };

        let mut image = PfsImage { reader, header, inodes: Vec::new(), root: 0 };
        image.load_inode_table()?;
        image.validate_inodes()?;
        image.root = image.resolve_root()?;
        Ok(Arc::new(image))
    }

    fn load_inode_table(&mut self) -> Result<()> {
        let h = self.header.clone();
        let isz = h.inode_size();
        let per_block = h.block_size / isz;
        if per_block == 0 {
            return corrupt("PFS block size is smaller than one inode");
        }
        let mut total = 0u64;
        for block in 0..h.inode_block_count {
            let base = h.block_size * (1 + block);
            if base + h.block_size > self.reader.len() {
                return corrupt("inode table extends past the end of the image");
            }
            let chunk = self.reader.read_at(base, h.block_size as usize)?;
            for j in 0..per_block {
                if total >= h.inode_count {
                    return Ok(());
                }
                let off = (j * isz) as usize;
                self.inodes
                    .push(Inode::parse(&chunk[off..off + isz as usize], total, &h)?);
                total += 1;
            }
        }
        Ok(())
    }

    /// Reject images whose inode table decrypted to noise. A wrong XTS key
    /// still leaves a valid plaintext superblock, so magic checks prove nothing.
    fn validate_inodes(&self) -> Result<()> {
        if self.inodes.is_empty() {
            return corrupt("PFS image contains no inodes");
        }
        let plausible = self.inodes.iter().filter(|i| i.is_dir() || i.is_file()).count();
        if plausible * 2 < self.inodes.len() {
            return corrupt(
                "inode table does not look like a filesystem (wrong key, or unsupported inode layout)",
            );
        }
        Ok(())
    }

    fn resolve_root(&self) -> Result<u64> {
        let mut number = self.header.superroot_inode;
        if number >= self.inodes.len() as u64 {
            number = 0;
        }
        let root = self.inode(number)?;
        let entries = self.listdir(&root)?;
        for e in &entries {
            if e.is_dir() && e.name == "uroot" {
                return Ok(self.inode(e.inode_number)?.number);
            }
        }
        // Some images place the content tree directly in the super root.
        if !entries.is_empty() {
            return Ok(root.number);
        }
        corrupt("PFS root directory is empty")
    }

    pub fn inode(&self, number: u64) -> Result<Inode> {
        match self.inodes.get(number as usize) {
            Some(i) => Ok(*i),
            None => corrupt(format!(
                "inode {number} is out of range (0..{})",
                self.inodes.len().saturating_sub(1)
            )),
        }
    }

    pub fn root(&self) -> Inode {
        self.inodes[self.root as usize]
    }

    pub fn block_map(&self, inode: &Inode) -> Result<BlockMap> {
        let h = &self.header;
        let count = inode.blocks as u64;
        if count == 0 {
            return Ok(BlockMap::List(Vec::new()));
        }
        if count > MAX_BLOCK_INDEX {
            return corrupt(format!("inode {} declares {count} blocks", inode.number));
        }
        if count == 1 {
            return Ok(BlockMap::List(vec![inode.direct[0]]));
        }
        if inode.direct[1] == 0xFFFF_FFFF || inode.direct[1] == u64::MAX {
            return Ok(BlockMap::Run { start: inode.direct[0], count });
        }

        let mut blocks: Vec<u64> = Vec::new();
        for &v in &inode.direct {
            if blocks.len() as u64 >= count {
                return Ok(BlockMap::List(blocks));
            }
            blocks.push(v);
        }

        let (entry_size, value_offset, value_size) = if h.signed() {
            if h.is_64bit() { (40, 32, 8) } else { (36, 32, 4) }
        } else {
            (4, 0, 4)
        };
        let per_block = h.block_size as usize / entry_size;
        let read_chain = |block: u64| -> Result<Vec<u64>> {
            let data = self.reader.read_at(block * h.block_size, h.block_size as usize)?;
            let mut out = Vec::new();
            for i in 0..per_block {
                let off = i * entry_size + value_offset;
                let v = if value_size == 8 { u64le(&data, off) } else { u32le(&data, off) as u64 };
                if v == 0 {
                    break;
                }
                out.push(v);
            }
            Ok(out)
        };

        if (blocks.len() as u64) < count && inode.indirect[0] != 0 {
            blocks.extend(read_chain(inode.indirect[0])?);
        }
        if (blocks.len() as u64) < count && inode.indirect[1] != 0 {
            for sub in read_chain(inode.indirect[1])? {
                if blocks.len() as u64 >= count {
                    break;
                }
                blocks.extend(read_chain(sub)?);
            }
        }
        if (blocks.len() as u64) < count {
            return corrupt(format!(
                "inode {} block map is incomplete ({} of {count})",
                inode.number,
                blocks.len()
            ));
        }
        blocks.truncate(count as usize);
        Ok(BlockMap::List(blocks))
    }

    /// Read `length` bytes of inode content block by block (directory data).
    fn inode_bytes(&self, inode: &Inode, length: u64) -> Result<Vec<u8>> {
        let bs = self.header.block_size;
        let map = self.block_map(inode)?;
        let mut out = Vec::new();
        for i in 0..map.len() {
            let take = bs.min(length.saturating_sub(out.len() as u64));
            if take == 0 {
                break;
            }
            let block = map.get(i).unwrap();
            out.extend(self.reader.read_at(block * bs, take as usize)?);
        }
        Ok(out)
    }

    pub fn listdir(&self, inode: &Inode) -> Result<Vec<Dirent>> {
        if !inode.is_dir() {
            return corrupt(format!("inode {} is not a directory", inode.number));
        }
        let bs = self.header.block_size;
        let mut span = inode.blocks as u64 * bs;
        if span == 0 {
            // Some images size directories but declare no block count.
            span = if inode.size == 0 { bs } else { inode.len() };
        }
        let data = self.inode_bytes(inode, span)?;
        let mut entries = Vec::new();
        let mut off = 0usize;
        while off + 16 <= data.len() {
            let inode_number = u32le(&data, off) as u64;
            let type_code = i32le(&data, off + 4);
            let name_length = i32le(&data, off + 8);
            let ent_size = i32le(&data, off + 12);
            if ent_size == 0 {
                break;
            }
            if ent_size < 16 || name_length < 0 || ent_size as usize > data.len() - off {
                return corrupt(format!(
                    "directory entry at 0x{off:X} in inode {} is malformed",
                    inode.number
                ));
            }
            let n = (name_length as usize).min(ent_size as usize - 16);
            let name = String::from_utf8_lossy(&data[off + 16..off + 16 + n]).into_owned();
            entries.push(Dirent { inode_number, type_code, name });
            off += ent_size as usize;
        }
        Ok(entries)
    }

    /// Directory entries without `.` and `..`.
    pub fn iter_dir(&self, inode: &Inode) -> Result<Vec<Dirent>> {
        Ok(self
            .listdir(inode)?
            .into_iter()
            .filter(|e| e.type_code != DIRENT_DOT && e.type_code != DIRENT_DOTDOT)
            .collect())
    }

    /// Resolve a path relative to `uroot`. `Ok(None)` means "not found".
    pub fn resolve(&self, path: &str) -> Result<Option<Inode>> {
        let normalized = path.replace('\\', "/");
        let mut parts: Vec<&str> = normalized.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
        if parts.first() == Some(&"uroot") {
            parts.remove(0);
        }
        let mut inode = self.root();
        for part in parts {
            if !inode.is_dir() {
                return Ok(None);
            }
            let Some(m) = self.iter_dir(&inode)?.into_iter().find(|e| e.name == part) else {
                return Ok(None);
            };
            inode = self.inode(m.inode_number)?;
        }
        Ok(Some(inode))
    }

    /// Depth-first walk of `uroot` yielding `(relative_path, inode)`.
    pub fn walk(&self) -> Result<Vec<(String, Inode)>> {
        const MAX_DEPTH: usize = 64;
        let root = self.root();
        let mut out = Vec::new();
        let mut stack: Vec<(String, Inode, usize)> = vec![(String::new(), root, 0)];
        let mut seen = std::collections::HashSet::from([root.number]);
        while let Some((prefix, inode, depth)) = stack.pop() {
            if depth > MAX_DEPTH {
                continue;
            }
            let mut dirs = Vec::new();
            for e in self.iter_dir(&inode)? {
                let child = self.inode(e.inode_number)?;
                let path = if prefix.is_empty() { e.name.clone() } else { format!("{prefix}/{}", e.name) };
                out.push((path.clone(), child));
                if child.is_dir() && seen.insert(child.number) {
                    dirs.push((path, child, depth + 1));
                }
            }
            stack.extend(dirs.into_iter().rev());
        }
        Ok(out)
    }

    /// Open a file inode for reading.
    pub fn open_file(self: &Arc<Self>, inode: &Inode, path: &str) -> Result<PfsFile> {
        if inode.is_dir() {
            return corrupt(format!("inode {} is a directory", inode.number));
        }
        let content = if inode.is_compressed() {
            if inode.size_compressed <= 0 {
                return corrupt(format!(
                    "inode {} is flagged compressed but stores no data",
                    inode.number
                ));
            }
            let stored = StoredData::new(self, inode, inode.size_compressed as u64)?;
            Content::Pfsc(PfscReader::new(Arc::new(stored), inode.len(), &format!("inode {}", inode.number))?)
        } else {
            Content::Plain(self.block_map(inode)?)
        };
        Ok(PfsFile { image: self.clone(), inode: *inode, content, name: path.to_string() })
    }

    pub fn find(self: &Arc<Self>, path: &str) -> Result<Option<PfsFile>> {
        match self.resolve(path)? {
            Some(inode) if !inode.is_dir() => Ok(Some(self.open_file(&inode, path)?)),
            _ => Ok(None),
        }
    }
}

fn validate_header(h: &PfsHeader) -> Result<()> {
    if h.block_size < 0x1000 || h.block_size > 0x100000 {
        return corrupt(format!("implausible PFS block size 0x{:X}", h.block_size));
    }
    if h.block_size % XTS_SECTOR_SIZE != 0 {
        return corrupt("PFS block size is not a multiple of the XTS sector size");
    }
    if h.inode_count > 50_000_000 {
        return corrupt(format!("implausible inode count {}", h.inode_count));
    }
    if h.inode_block_count > MAX_INODE_BLOCKS {
        return corrupt(format!("implausible inode block count {}", h.inode_block_count));
    }
    Ok(())
}

enum Content {
    Plain(BlockMap),
    Pfsc(PfscReader),
}

/// A readable file inside a PFS image. As a [`Source`] it is strict; use
/// [`PfsFile::read_range`] for EOF-clamped reads.
pub struct PfsFile {
    image: Arc<PfsImage>,
    pub inode: Inode,
    content: Content,
    name: String,
}

impl PfsFile {
    pub fn size(&self) -> u64 {
        self.inode.len()
    }

    fn read_plain(&self, map: &BlockMap, offset: u64, buf: &mut [u8]) -> Result<()> {
        let bs = self.image.header.block_size;
        let mut done = 0usize;
        while done < buf.len() {
            let pos = offset + done as u64;
            let (index, skip) = (pos / bs, pos % bs);
            let take = ((bs - skip) as usize).min(buf.len() - done);
            match map.get(index) {
                Some(block) => self.image.reader.read_into(block * bs + skip, &mut buf[done..done + take])?,
                // Past the block map: sparse tail, reads as zeros.
                None => buf[done..done + take].fill(0),
            }
            done += take;
        }
        Ok(())
    }
}

impl Source for PfsFile {
    fn len(&self) -> u64 {
        self.size()
    }

    fn read_into(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        check_bounds(&self.name, offset, buf.len(), self.size())?;
        match &self.content {
            Content::Plain(map) => self.read_plain(map, offset, buf),
            Content::Pfsc(r) => r.read_into(offset, buf),
        }
    }

    fn name(&self) -> &str {
        &self.name
    }
}

/// Strict view of the bytes an inode stores on disk (for a compressed inode,
/// the PFSC stream, not the decoded file).
struct StoredData {
    reader: SharedSource,
    blocks: BlockMap,
    block_size: u64,
    size: u64,
    name: String,
}

impl StoredData {
    fn new(image: &PfsImage, inode: &Inode, size: u64) -> Result<Self> {
        let blocks = image.block_map(inode)?;
        let bs = image.header.block_size;
        if blocks.len() * bs < size {
            return corrupt(format!(
                "inode {}: stores {size} bytes but maps only {} block(s)",
                inode.number,
                blocks.len()
            ));
        }
        Ok(StoredData {
            reader: image.reader.clone(),
            blocks,
            block_size: bs,
            size,
            name: format!("inode {}", inode.number),
        })
    }
}

impl Source for StoredData {
    fn len(&self) -> u64 {
        self.size
    }

    fn read_into(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        if offset.checked_add(buf.len() as u64).map_or(true, |e| e > self.size) {
            return corrupt(format!(
                "{}: need {} bytes at 0x{offset:X} of a 0x{:X}-byte stored stream",
                self.name,
                buf.len(),
                self.size
            ));
        }
        let bs = self.block_size;
        let mut done = 0usize;
        while done < buf.len() {
            let pos = offset + done as u64;
            let (index, skip) = (pos / bs, pos % bs);
            let take = ((bs - skip) as usize).min(buf.len() - done);
            let block = self.blocks.get(index).expect("checked against the block count");
            self.reader.read_into(block * bs + skip, &mut buf[done..done + take])?;
            done += take;
        }
        Ok(())
    }

    fn name(&self) -> &str {
        &self.name
    }
}

/// Random access to a PFSC block-compressed file. One decoded block is cached,
/// so memory use is bounded by the block size. Every inconsistency is an error.
struct PfscReader {
    stored: SharedSource,
    size: u64,
    block_size: u64,
    count: u64,
    table: u64,
    data_start: u64,
    name: String,
    cache: Mutex<Option<(u64, Arc<Vec<u8>>)>>,
}

impl PfscReader {
    fn new(stored: SharedSource, size: u64, name: &str) -> Result<Self> {
        let stored_len = stored.len();
        if stored_len < PFSC_HEADER_SIZE {
            return corrupt(format!("{name}: PFSC header is truncated"));
        }
        let h = stored.read_at(0, PFSC_HEADER_SIZE as usize)?;
        if &h[..4] != PFSC_MAGIC {
            return corrupt(format!("{name}: compressed file has no PFSC magic (got {:02X?})", &h[..4]));
        }
        let block_size = u32le(&h, 0x0C) as u64;
        if block_size == 0 || block_size > PFSC_MAX_BLOCK || u64le(&h, 0x10) != block_size {
            return corrupt(format!("{name}: implausible PFSC block size 0x{block_size:X}"));
        }
        let table = u64le(&h, 0x18);
        let data_start = u64le(&h, 0x20);
        let data_length = u64le(&h, 0x28);
        if data_length != size {
            return corrupt(format!("{name}: PFSC header says {data_length} bytes, inode says {size}"));
        }
        let count = size.div_ceil(block_size);
        let table_end = (count + 1).checked_mul(8).and_then(|n| n.checked_add(table));
        let Some(table_end) = table_end.filter(|&e| table >= PFSC_HEADER_SIZE && e <= stored_len) else {
            return corrupt(format!(
                "{name}: PFSC block table at 0x{table:X} runs past the 0x{stored_len:X}-byte stream"
            ));
        };
        if data_start < table_end || data_start > stored_len {
            return corrupt(format!("{name}: PFSC data start 0x{data_start:X} is out of range"));
        }
        Ok(PfscReader {
            stored,
            size,
            block_size,
            count,
            table,
            data_start,
            name: name.to_string(),
            cache: Mutex::new(None),
        })
    }

    fn block(&self, index: u64) -> Result<Arc<Vec<u8>>> {
        if let Some((i, data)) = self.cache.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            if *i == index {
                return Ok(data.clone());
            }
        }
        if index >= self.count {
            return corrupt(format!("{}: PFSC block {index} is out of range", self.name));
        }
        let expected = self.block_size.min(self.size - index * self.block_size) as usize;
        let raw = self.stored.read_at(self.table + index * 8, 16)?;
        let (start, end) = (u64le(&raw, 0), u64le(&raw, 8));
        if !(self.data_start <= start && start < end && end <= self.stored.len()) {
            return corrupt(format!(
                "{}: PFSC block {index} spans 0x{start:X}..0x{end:X}, outside the data area 0x{:X}..0x{:X}",
                self.name,
                self.data_start,
                self.stored.len()
            ));
        }
        if end - start > self.block_size {
            return corrupt(format!(
                "{}: PFSC block {index} stores {} bytes, more than the 0x{:X} block size",
                self.name,
                end - start,
                self.block_size
            ));
        }
        let data = self.stored.read_at(start, (end - start) as usize)?;
        let mut decoded = if data.len() as u64 == self.block_size {
            data
        } else {
            inflate(&data, self.block_size as usize, &format!("{}: PFSC block {index}", self.name))?
        };
        // The final block may be padded out to the full block size.
        let last_padded = index == self.count - 1 && decoded.len() as u64 == self.block_size;
        if decoded.len() != expected && !last_padded {
            return corrupt(format!(
                "{}: PFSC block {index} decodes to {} bytes (expected {expected})",
                self.name,
                decoded.len()
            ));
        }
        decoded.truncate(expected);
        let decoded = Arc::new(decoded);
        *self.cache.lock().unwrap_or_else(|e| e.into_inner()) = Some((index, decoded.clone()));
        Ok(decoded)
    }

    fn read_into(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let mut done = 0usize;
        while done < buf.len() {
            let pos = offset + done as u64;
            let (index, skip) = (pos / self.block_size, (pos % self.block_size) as usize);
            let block = self.block(index)?;
            let take = (block.len() - skip).min(buf.len() - done);
            buf[done..done + take].copy_from_slice(&block[skip..skip + take]);
            done += take;
        }
        Ok(())
    }
}

fn looks_like_zlib(d: &[u8]) -> bool {
    if d.len() < 2 {
        return false;
    }
    let (cmf, flg) = (d[0] as u32, d[1] as u32);
    cmf & 0x0F == 8 && cmf >> 4 <= 7 && flg & 0x20 == 0 && (cmf << 8 | flg) % 31 == 0
}

/// Decode one zlib or raw-deflate block of at most `limit` bytes, or fail.
fn inflate(data: &[u8], limit: usize, label: &str) -> Result<Vec<u8>> {
    let attempts: &[bool] = if looks_like_zlib(data) { &[true, false] } else { &[false] };
    let mut error = String::from("not a deflate stream");
    for &zlib_header in attempts {
        let mut d = Decompress::new(zlib_header);
        let mut out = vec![0u8; limit + 1];
        let status = match d.decompress(data, &mut out, FlushDecompress::Finish) {
            Ok(s) => s,
            Err(e) => {
                error = e.to_string();
                continue;
            }
        };
        let produced = d.total_out() as usize;
        if produced > limit {
            return corrupt(format!("{label} inflates past the 0x{limit:X} block size"));
        }
        if status != Status::StreamEnd {
            error = "stream is truncated".into();
            continue;
        }
        if data[d.total_in() as usize..].iter().any(|&b| b != 0) {
            error = "trailing bytes after the deflate stream".into();
            continue;
        }
        out.truncate(produced);
        return Ok(out);
    }
    corrupt(format!("{label} is not valid compressed data ({error})"))
}
