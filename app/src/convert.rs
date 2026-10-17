//! High-level operations shared by the window app and the CLI: load a file,
//! pick the executable inside it, convert to ELF, extract everything.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::binary::{fmt_size, Source};
use crate::elf::{is_elf, is_self, to_elf, Conversion};
use crate::error::{Error, Result};
use crate::pfs::PfsImage;
use crate::pkg::{content_type_name, Kind, Pkg, CNT_MAGIC, FIH_MAGIC};

/// Preferred in-package targets for the ELF conversion, best first.
const ELF_CANDIDATES: [&str; 3] = ["eboot.bin", "sce_sys/about/right.sprx", "sce_sys/eboot.bin"];
/// Suffixes worth sniffing when no well-known name is present.
const ELF_SUFFIXES: [&str; 5] = [".elf", ".prx", ".sprx", ".bin", ".self"];
const CHUNK: usize = 1 << 20;

/// Progress callback: (bytes done, bytes total, current item).
pub type Progress<'a> = &'a dyn Fn(u64, u64, &str);

#[derive(Clone)]
pub enum Input {
    /// An opened package filesystem.
    Package(Arc<PfsImage>),
    /// A standalone SELF or ELF file.
    Binary(PathBuf),
}

#[derive(Clone, Debug)]
pub struct FileRow {
    pub path: String,
    pub size: u64,
    pub is_dir: bool,
}

pub struct Loaded {
    /// Label/value pairs describing the file.
    pub rows: Vec<(String, String)>,
    /// `None` when the package needs a (different) passcode.
    pub input: Option<Input>,
    pub files: Vec<FileRow>,
    /// The file "Convert to ELF" picks by default.
    pub default_elf: Option<String>,
    /// Why the filesystem could not be opened, if it is a key problem.
    pub key_problem: Option<Error>,
}

pub fn load(path: &Path, passcode: Option<&str>) -> Result<Loaded> {
    let mut magic = [0u8; 4];
    {
        use std::io::Read;
        let mut f = fs::File::open(path)?;
        let n = f.read(&mut magic)?;
        if n < 4 {
            return Err(Error::NotAPackage(format!("{} is too small to be a package", path.display())));
        }
    }

    if &magic == CNT_MAGIC || &magic == FIH_MAGIC {
        let pkg = Pkg::open(path)?;
        let mut rows = package_rows(&pkg);
        let pfs = match pkg.open_pfs(passcode) {
            Ok(p) => p,
            Err(e) if e.needs_key() => {
                return Ok(Loaded { rows, input: None, files: Vec::new(), default_elf: None, key_problem: Some(e) })
            }
            Err(e) => return Err(e),
        };
        let h = &pfs.header;
        rows.push((
            "Filesystem".into(),
            format!(
                "PFS v{}, {}{} inodes",
                h.version,
                if h.encrypted() { "encrypted, " } else { "" },
                pfs.inodes.len()
            ),
        ));
        let files: Vec<FileRow> = pfs
            .walk()?
            .into_iter()
            .map(|(path, i)| FileRow { path, size: i.len(), is_dir: i.is_dir() })
            .collect();
        let default_elf = first_elf_candidate(&pfs, &files)?;
        rows.push(("Files".into(), files.iter().filter(|f| !f.is_dir).count().to_string()));
        return Ok(Loaded { rows, input: Some(Input::Package(pfs)), files, default_elf, key_problem: None });
    }

    let head = magic.to_vec();
    if !(is_elf(&head) || is_self(&head)) {
        return Err(Error::Conversion(format!(
            "{} is neither a package nor an ELF/SELF file",
            path.display()
        )));
    }
    let size = fs::metadata(path)?.len();
    let rows = vec![
        ("Format".into(), if is_self(&head) { "SELF (signed ELF)".into() } else { "ELF".into() }),
        ("Size".into(), fmt_size(size)),
    ];
    Ok(Loaded { rows, input: Some(Input::Binary(path.to_path_buf())), files: Vec::new(), default_elf: None, key_problem: None })
}

fn package_rows(pkg: &Pkg) -> Vec<(String, String)> {
    let h = &pkg.container.header;
    let mut rows = vec![
        ("Format".to_string(), format!("{} package", pkg.kind.label())),
        ("Content ID".to_string(), h.content_id.clone()),
    ];
    if pkg.kind == Kind::Ps4 {
        rows.push((
            "Content type".into(),
            format!("0x{:02X} {}", h.content_type, content_type_name(h.content_type)),
        ));
    }
    if let Some(f) = &pkg.fih {
        rows.push(("Variant".into(), if f.retail() { "retail".into() } else { "debug".into() }));
    }
    rows.push(("Entries".into(), pkg.container.entries.len().to_string()));
    rows
}

fn sniff(pfs: &Arc<PfsImage>, path: &str) -> Result<bool> {
    let Some(file) = pfs.find(path)? else { return Ok(false) };
    let n = file.size().min(8) as usize;
    let head = match file.read_at(0, n) {
        Ok(h) => h,
        Err(_) => return Ok(false),
    };
    Ok(is_elf(&head) || is_self(&head))
}

fn first_elf_candidate(pfs: &Arc<PfsImage>, files: &[FileRow]) -> Result<Option<String>> {
    for name in ELF_CANDIDATES {
        if sniff(pfs, name)? {
            return Ok(Some(name.to_string()));
        }
    }
    let mut others: Vec<&FileRow> = files
        .iter()
        .filter(|f| !f.is_dir && !ELF_CANDIDATES.contains(&f.path.as_str()))
        .filter(|f| {
            let lower = f.path.to_lowercase();
            ELF_SUFFIXES.iter().any(|s| lower.ends_with(s))
        })
        .collect();
    others.sort_by(|a, b| (a.path.matches('/').count(), &a.path).cmp(&(b.path.matches('/').count(), &b.path)));
    for f in others {
        if sniff(pfs, &f.path)? {
            return Ok(Some(f.path.clone()));
        }
    }
    Ok(None)
}

pub struct ConvertReport {
    pub source: String,
    pub output: PathBuf,
    pub conversion: Conversion,
}

fn read_source(src: &dyn Source, label: &str, progress: Progress) -> Result<Vec<u8>> {
    let total = src.len();
    let mut out = Vec::with_capacity(total as usize);
    let mut buf = vec![0u8; CHUNK];
    while (out.len() as u64) < total {
        let take = (total - out.len() as u64).min(CHUNK as u64) as usize;
        src.read_into(out.len() as u64, &mut buf[..take])?;
        out.extend_from_slice(&buf[..take]);
        progress(out.len() as u64, total, label);
    }
    Ok(out)
}

/// Convert the chosen file (or the default executable) to a standard ELF and
/// write it to `output`. Nothing is written unless conversion succeeds.
pub fn convert_to_elf(input: &Input, inner_path: Option<&str>, output: &Path, progress: Progress) -> Result<ConvertReport> {
    let (source, data) = match input {
        Input::Binary(path) => {
            let data = fs::read(path)?;
            progress(data.len() as u64, data.len() as u64, "reading");
            (path.display().to_string(), data)
        }
        Input::Package(pfs) => {
            let path = match inner_path {
                Some(p) => p.to_string(),
                None => {
                    let files: Vec<FileRow> = pfs
                        .walk()?
                        .into_iter()
                        .map(|(path, i)| FileRow { path, size: i.len(), is_dir: i.is_dir() })
                        .collect();
                    first_elf_candidate(pfs, &files)?.ok_or_else(|| {
                        Error::Conversion("no ELF or SELF file was found in the package; pick one from the list".into())
                    })?
                }
            };
            let Some(file) = pfs.find(&path)? else {
                if pfs.resolve(&path)?.is_some() {
                    return Err(Error::Conversion(format!("{path} is a directory, not a file")));
                }
                return Err(Error::Conversion(format!("{path} was not found in the package filesystem")));
            };
            let data = read_source(&file, &path, progress)?;
            (path, data)
        }
    };
    if !(is_elf(&data) || is_self(&data)) {
        return Err(Error::Conversion(format!("{source} is not an ELF or SELF file")));
    }
    let conversion = to_elf(&data)?;
    fs::write(output, &conversion.data)?;
    Ok(ConvertReport { source, output: output.to_path_buf(), conversion })
}

/// Default output file name for a converted executable.
pub fn default_output_name(source: &str) -> String {
    let base = source.rsplit(['/', '\\']).next().unwrap_or(source);
    match base.rsplit_once('.') {
        Some((stem, ext)) if ["bin", "self", "sprx", "prx", "elf"].contains(&ext.to_lowercase().as_str()) => {
            format!("{stem}.elf")
        }
        _ => format!("{base}.elf"),
    }
}

/// Refuse paths that would escape the destination folder.
pub fn safe_relative(path: &str) -> Result<PathBuf> {
    let parts: Vec<&str> = path.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return Err(Error::Corrupt(format!("refusing to write empty path from {path:?}")));
    }
    for p in &parts {
        if *p == "." || *p == ".." || p.contains(':') {
            return Err(Error::Corrupt(format!("refusing to write suspicious path {path:?}")));
        }
    }
    Ok(parts.iter().collect())
}

/// Extract every file in the package into `out_dir`. Returns the file count.
pub fn extract_all(pfs: &Arc<PfsImage>, out_dir: &Path, progress: Progress) -> Result<usize> {
    let entries = pfs.walk()?;
    let total: u64 = entries.iter().filter(|(_, i)| i.is_file()).map(|(_, i)| i.len()).sum();
    let mut done = 0u64;
    let mut written = 0usize;
    let mut buf = vec![0u8; CHUNK];
    for (path, inode) in &entries {
        let dest = out_dir.join(safe_relative(path)?);
        if inode.is_dir() {
            fs::create_dir_all(&dest)?;
            continue;
        }
        if !inode.is_file() {
            continue;
        }
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = pfs.open_file(inode, path)?;
        let result = (|| -> Result<()> {
            let mut out = std::io::BufWriter::new(fs::File::create(&dest)?);
            let mut pos = 0u64;
            while pos < file.size() {
                let take = (file.size() - pos).min(CHUNK as u64) as usize;
                file.read_into(pos, &mut buf[..take])?;
                out.write_all(&buf[..take])?;
                pos += take as u64;
                progress(done + pos, total, path);
            }
            out.flush()?;
            Ok(())
        })();
        if let Err(e) = result {
            // Never leave a half-written file that looks complete.
            let _ = fs::remove_file(&dest);
            return Err(e);
        }
        done += file.size();
        written += 1;
    }
    progress(total, total, "done");
    Ok(written)
}
