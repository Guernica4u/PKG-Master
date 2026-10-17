//! PS4 `\x7FCNT` and PS5 `\x7FFIH` package containers.
//!
//! PS4: a big-endian CNT header and entry table; the PFS image is at
//! `pfs_image_offset`. PS5: a little-endian FIH envelope holding an outer PFS,
//! an embedded CNT, and install metadata. The application filesystem is the
//! `pfs_image.dat` file *inside* the outer PFS.

use std::path::Path;
use std::sync::Arc;

use crate::binary::{ascii_z, u32be, u64be, u64le, FileSource, SharedSource, Source, Window};
use crate::crypto::{compute_keys, compute_keys_sha3};
use crate::error::{Error, Result};
use crate::pfs::PfsImage;

pub const CNT_MAGIC: &[u8; 4] = b"\x7fCNT";
pub const FIH_MAGIC: &[u8; 4] = b"\x7fFIH";
const HEADER_SIZE: usize = 0x5A0;
const ENTRY_SIZE: u64 = 0x20;
const PFS_NEW_CRYPT_FLAG: u64 = 0x2000_0000_0000_0000;

pub fn entry_id_name(id: u32) -> &'static str {
    match id {
        0x0001 => "digests",
        0x0010 => "entry_keys",
        0x0020 => "image_key",
        0x0080 => "general_digests",
        0x0100 => "metas",
        0x0200 => "entry_names",
        0x0400 => "license.dat",
        0x0401 => "license.info",
        0x0402 => "nptitle.dat",
        0x0403 => "npbind.dat",
        0x0404 => "selfinfo.dat",
        0x040A => "imagedigs.dat",
        0x1000 => "param.sfo",
        0x1001 => "playgo-chunk.dat",
        0x1200 => "icon0.png",
        0x1220 => "pic0.png",
        0x1240 => "snd0.at9",
        0x2000 => "param.json",
        _ => "",
    }
}

pub fn content_type_name(t: u32) -> &'static str {
    match t {
        0x1A => "GD (game data)",
        0x1B => "AC (additional content)",
        0x1C => "AL (additional license)",
        0x1E => "DP (delta patch)",
        _ => "unknown",
    }
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub id: u32,
    pub name_offset: u32,
    pub flags1: u32,
    pub flags2: u32,
    pub data_offset: u32,
    pub data_size: u32,
}

impl Entry {
    pub fn encrypted(&self) -> bool {
        self.flags1 & 0x8000_0000 != 0
    }
}

#[derive(Clone, Debug)]
pub struct PkgHeader {
    pub entry_count: u32,
    pub entry_table_offset: u32,
    pub body_offset: u64,
    pub body_size: u64,
    pub content_id: String,
    pub drm_type: u32,
    pub content_type: u32,
    pub pfs_flags: u64,
    pub pfs_image_offset: u64,
    pub pfs_image_size: u64,
    pub package_size: u64,
}

impl PkgHeader {
    fn parse(d: &[u8]) -> Result<Self> {
        if d.len() < HEADER_SIZE {
            return Err(Error::Truncated(format!("package header needs 0x{HEADER_SIZE:X} bytes")));
        }
        if &d[..4] != CNT_MAGIC {
            return Err(Error::NotAPackage(format!("expected 7F434E54 magic, got {:02X?}", &d[..4])));
        }
        Ok(PkgHeader {
            entry_count: u32be(d, 0x10),
            entry_table_offset: u32be(d, 0x18),
            body_offset: u64be(d, 0x20),
            body_size: u64be(d, 0x28),
            content_id: ascii_z(d, 0x40, 0x30),
            drm_type: u32be(d, 0x70),
            content_type: u32be(d, 0x74),
            pfs_flags: u64be(d, 0x408),
            pfs_image_offset: u64be(d, 0x410),
            pfs_image_size: u64be(d, 0x418),
            package_size: u64be(d, 0x430),
        })
    }

    pub fn new_crypt(&self) -> bool {
        self.pfs_flags & PFS_NEW_CRYPT_FLAG != 0
    }
}

#[derive(Clone, Debug)]
pub struct FihHeader {
    pub signed_flag: u8,
    pub pfs_offset: u64,
    pub pfs_size: u64,
    pub cnt_offset: u64,
    pub cnt_size: u64,
}

impl FihHeader {
    fn parse(d: &[u8]) -> Result<Self> {
        if d.len() < 0xB0 {
            return Err(Error::Truncated("PS5 FIH header needs at least 0xB0 bytes".into()));
        }
        Ok(FihHeader {
            signed_flag: d[5],
            pfs_offset: u64le(d, 0x10),
            pfs_size: u64le(d, 0x18),
            cnt_offset: u64le(d, 0x58),
            cnt_size: u64le(d, 0xA0),
        })
    }

    pub fn retail(&self) -> bool {
        self.signed_flag & 0x80 != 0
    }
}

/// Entry table and name table of a CNT container.
pub struct Container {
    pub header: PkgHeader,
    pub entries: Vec<Entry>,
    names: Vec<String>,
}

impl Container {
    fn parse(reader: &dyn Source) -> Result<Self> {
        let header = PkgHeader::parse(&reader.read_at(0, HEADER_SIZE)?)?;
        if header.entry_count > 1_000_000 {
            return Err(Error::Corrupt(format!("implausible entry count {}", header.entry_count)));
        }
        let table_len = header.entry_count as u64 * ENTRY_SIZE;
        if header.entry_table_offset as u64 + table_len > reader.len() {
            return Err(Error::Corrupt(format!(
                "entry table at 0x{:X} for {} entries runs past the container",
                header.entry_table_offset, header.entry_count
            )));
        }
        let raw = reader.read_at(header.entry_table_offset as u64, table_len as usize)?;
        let entries: Vec<Entry> = raw
            .chunks_exact(ENTRY_SIZE as usize)
            .map(|r| Entry {
                id: u32be(r, 0x00),
                name_offset: u32be(r, 0x04),
                flags1: u32be(r, 0x08),
                flags2: u32be(r, 0x0C),
                data_offset: u32be(r, 0x10),
                data_size: u32be(r, 0x14),
            })
            .collect();
        let mut names = Vec::new();
        if let Some(e) = entries.iter().find(|e| e.id == 0x0200) {
            if e.data_offset as u64 + e.data_size as u64 > reader.len() {
                return Err(Error::Corrupt("entry name table is out of bounds".into()));
            }
            let blob = reader.read_at(e.data_offset as u64, e.data_size as usize)?;
            names = blob.split(|&b| b == 0).map(|p| String::from_utf8_lossy(p).into_owned()).collect();
        }
        Ok(Container { header, entries, names })
    }

    pub fn name_of(&self, e: &Entry) -> String {
        let i = e.name_offset as usize;
        if i > 0 && i < self.names.len() && !self.names[i].is_empty() {
            return self.names[i].clone();
        }
        entry_id_name(e.id).to_string()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Ps4,
    Ps5,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Ps4 => "PS4",
            Kind::Ps5 => "PS5",
        }
    }
}

pub struct Pkg {
    reader: SharedSource,
    /// The CNT container (the whole file on PS4, a window into it on PS5).
    cnt: SharedSource,
    pub kind: Kind,
    pub container: Container,
    pub fih: Option<FihHeader>,
}

impl Pkg {
    pub fn open(path: &Path) -> Result<Self> {
        let reader: SharedSource = Arc::new(FileSource::open(path)?);
        if reader.len() < 4 {
            return Err(Error::NotAPackage(format!("{}: file is too small to be a package", path.display())));
        }
        let magic = reader.read_at(0, 4)?;
        if magic == CNT_MAGIC {
            let container = Container::parse(reader.as_ref())?;
            return Ok(Pkg { cnt: reader.clone(), reader, kind: Kind::Ps4, container, fih: None });
        }
        if magic == FIH_MAGIC {
            let fih = FihHeader::parse(&reader.read_at(0, 0x100.min(reader.len() as usize))?)?;
            if fih.cnt_size == 0 || fih.cnt_offset.saturating_add(fih.cnt_size) > reader.len() {
                return Err(Error::Corrupt("PS5 image has no usable embedded metadata container".into()));
            }
            let cnt: SharedSource = Arc::new(Window::new(reader.clone(), fih.cnt_offset, fih.cnt_size, "ps5-cnt")?);
            let container = Container::parse(cnt.as_ref())?;
            return Ok(Pkg { reader, cnt, kind: Kind::Ps5, container, fih: Some(fih) });
        }
        Err(Error::NotAPackage(format!(
            "{}: magic {:02X?} is neither PS4 \\x7FCNT nor PS5 \\x7FFIH",
            path.display(),
            magic
        )))
    }

    /// The payload of an unencrypted metadata entry (e.g. 0x1000 param.sfo),
    /// or `None` if the entry is missing, encrypted or implausibly large.
    pub fn entry_data(&self, id: u32) -> Result<Option<Vec<u8>>> {
        let Some(e) = self.container.entries.iter().find(|e| e.id == id) else { return Ok(None) };
        if e.encrypted() || e.data_size == 0 || e.data_size as u64 > crate::meta::MAX_META_SIZE {
            return Ok(None);
        }
        if e.data_offset as u64 + e.data_size as u64 > self.cnt.len() {
            return Err(Error::Corrupt(format!("entry 0x{:04X} runs past the container", e.id)));
        }
        Ok(Some(self.cnt.read_at(e.data_offset as u64, e.data_size as usize)?))
    }

    pub fn content_id(&self) -> &str {
        &self.container.header.content_id
    }

    /// The region that holds the (outermost) PFS data.
    fn pfs_region(&self) -> Result<Option<SharedSource>> {
        let len = self.reader.len();
        let window = |off: u64, size: u64, name: &str| -> Result<Option<SharedSource>> {
            Ok(Some(Arc::new(Window::new(self.reader.clone(), off, size, name)?)))
        };
        match self.kind {
            Kind::Ps4 => {
                let h = &self.container.header;
                if h.pfs_image_size != 0 && h.pfs_image_offset.saturating_add(h.pfs_image_size) <= len {
                    return window(h.pfs_image_offset, h.pfs_image_size, "ps4-pfs");
                }
                if h.body_size != 0 && h.body_offset.saturating_add(h.body_size) <= len {
                    return window(h.body_offset, h.body_size, "ps4-body");
                }
                Ok(None)
            }
            Kind::Ps5 => {
                let f = self.fih.as_ref().expect("PS5 package has a FIH header");
                if f.pfs_size == 0 {
                    return Ok(None);
                }
                window(f.pfs_offset, f.pfs_size, "ps5-outer-pfs")
            }
        }
    }

    /// Possible EKPFS values, in the order they should be tried.
    fn key_candidates(&self, passcode: Option<&str>) -> Result<Vec<Option<[u8; 32]>>> {
        match passcode.filter(|p| !p.is_empty()) {
            Some(p) => Ok(vec![
                Some(compute_keys(self.content_id(), p, 1)?),
                Some(compute_keys_sha3(self.content_id(), p, 1)?),
            ]),
            None => Ok(vec![None]),
        }
    }

    /// Open the application filesystem, trying every key and crypt-schedule
    /// combination available.
    pub fn open_pfs(&self, passcode: Option<&str>) -> Result<Arc<PfsImage>> {
        let region = self
            .pfs_region()?
            .ok_or_else(|| Error::Corrupt("package does not reference a PFS image".into()))?;
        let keys = self.key_candidates(passcode)?;
        let mut last: Option<Error> = None;
        for key in &keys {
            for new_crypt in [false, true] {
                let attempt = match self.kind {
                    Kind::Ps4 => PfsImage::open(region.clone(), key.as_ref(), new_crypt),
                    Kind::Ps5 => open_ps5_inner(region.clone(), key.as_ref(), new_crypt),
                };
                match attempt {
                    Ok(img) => return Ok(img),
                    Err(e @ (Error::KeyRequired(_) | Error::Corrupt(_))) => last = Some(e),
                    Err(e) => return Err(e),
                }
            }
        }
        if keys.iter().all(Option::is_none) {
            return Err(Error::KeyRequired(
                "the PFS image is encrypted; enter the package passcode to read it".into(),
            ));
        }
        match last {
            Some(e @ Error::KeyRequired(_)) => Err(e),
            Some(e) => Err(Error::WrongKey(format!("no supplied key could read the PFS image: {e}"))),
            None => Err(Error::Corrupt("unable to open the PFS image".into())),
        }
    }
}

fn open_ps5_inner(region: SharedSource, key: Option<&[u8; 32]>, new_crypt: bool) -> Result<Arc<PfsImage>> {
    let outer = PfsImage::open(region, key, new_crypt)?;
    let node = match outer.find("pfs_image.dat")? {
        Some(n) => Some(n),
        None => outer.find("image0/pfs_image.dat")?,
    };
    let Some(node) = node else {
        return Err(Error::Corrupt(
            "outer PS5 image has no pfs_image.dat (is the inner image compressed?)".into(),
        ));
    };
    PfsImage::open(Arc::new(node), key, new_crypt)
}
