//! Positional, bounds-checked readers.
//!
//! Every parser works through [`Source`], so the package file, a PFS image
//! inside it, or a single file inside a PFS image compose without copying.
//! Sources are strict: a read past the end is an error, never a short read.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::error::{Error, Result};

pub trait Source: Send + Sync {
    fn len(&self) -> u64;
    /// Fill `buf` from `offset`, or fail if that runs past the end.
    fn read_into(&self, offset: u64, buf: &mut [u8]) -> Result<()>;
    fn name(&self) -> &str {
        "input"
    }

    fn read_at(&self, offset: u64, length: usize) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; length];
        self.read_into(offset, &mut buf)?;
        Ok(buf)
    }
}

pub type SharedSource = Arc<dyn Source>;

pub fn check_bounds(name: &str, offset: u64, length: usize, size: u64) -> Result<()> {
    match offset.checked_add(length as u64) {
        Some(end) if end <= size => Ok(()),
        _ => Err(Error::Truncated(format!(
            "{name}: need {length} bytes at 0x{offset:X}, but the window is only 0x{size:X} bytes"
        ))),
    }
}

pub struct FileSource {
    file: Mutex<File>,
    size: u64,
    name: String,
}

impl FileSource {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        let size = file.metadata()?.len();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        Ok(FileSource { file: Mutex::new(file), size, name })
    }
}

impl Source for FileSource {
    fn len(&self) -> u64 {
        self.size
    }

    fn read_into(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        check_bounds(&self.name, offset, buf.len(), self.size)?;
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(buf).map_err(|e| {
            Error::Truncated(format!("{}: short read at 0x{offset:X}: {e}", self.name))
        })
    }

    fn name(&self) -> &str {
        &self.name
    }
}

/// A bounded sub-window over another source.
pub struct Window {
    parent: SharedSource,
    offset: u64,
    size: u64,
    name: String,
}

impl Window {
    pub fn new(parent: SharedSource, offset: u64, size: u64, name: &str) -> Result<Self> {
        match offset.checked_add(size) {
            Some(end) if end <= parent.len() => {}
            _ => {
                return Err(Error::Truncated(format!(
                    "{name}: window 0x{offset:X}+0x{size:X} exceeds 0x{:X}",
                    parent.len()
                )))
            }
        }
        Ok(Window { parent, offset, size, name: name.to_string() })
    }
}

impl Source for Window {
    fn len(&self) -> u64 {
        self.size
    }

    fn read_into(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        check_bounds(&self.name, offset, buf.len(), self.size)?;
        self.parent.read_into(self.offset + offset, buf)
    }

    fn name(&self) -> &str {
        &self.name
    }
}

/// An in-memory source, used for standalone SELF/ELF files and tests.
pub struct MemSource {
    pub data: Vec<u8>,
    pub name: String,
}

impl Source for MemSource {
    fn len(&self) -> u64 {
        self.data.len() as u64
    }

    fn read_into(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        check_bounds(&self.name, offset, buf.len(), self.len())?;
        let start = offset as usize;
        buf.copy_from_slice(&self.data[start..start + buf.len()]);
        Ok(())
    }

    fn name(&self) -> &str {
        &self.name
    }
}

pub fn u16le(d: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([d[o], d[o + 1]])
}
pub fn u32le(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(d[o..o + 4].try_into().unwrap())
}
pub fn u64le(d: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(d[o..o + 8].try_into().unwrap())
}
pub fn i32le(d: &[u8], o: usize) -> i32 {
    i32::from_le_bytes(d[o..o + 4].try_into().unwrap())
}
pub fn i64le(d: &[u8], o: usize) -> i64 {
    i64::from_le_bytes(d[o..o + 8].try_into().unwrap())
}
pub fn u32be(d: &[u8], o: usize) -> u32 {
    u32::from_be_bytes(d[o..o + 4].try_into().unwrap())
}
pub fn u64be(d: &[u8], o: usize) -> u64 {
    u64::from_be_bytes(d[o..o + 8].try_into().unwrap())
}

/// Decode a NUL-terminated ASCII string from a fixed-size field.
pub fn ascii_z(d: &[u8], offset: usize, length: usize) -> String {
    let end = (offset + length).min(d.len());
    let raw = &d[offset.min(end)..end];
    let raw = match raw.iter().position(|&b| b == 0) {
        Some(n) => &raw[..n],
        None => raw,
    };
    String::from_utf8_lossy(raw).into_owned()
}

pub fn fmt_size(n: u64) -> String {
    let mut value = n as f64;
    for unit in ["B", "KiB", "MiB", "GiB"] {
        if value < 1024.0 || unit == "GiB" {
            return if unit == "B" { format!("{n} B") } else { format!("{value:.1} {unit}") };
        }
        value /= 1024.0;
    }
    unreachable!()
}
