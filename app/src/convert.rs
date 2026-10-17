//! High-level operations shared by the window app and the CLI: load a file,
//! pick the executable inside it, convert to ELF, extract everything, or lay
//! out a game for AnyPS5.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::binary::{fmt_size, Source};
use crate::elf::{alignment_problems, is_elf, is_self, to_elf, Conversion};
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
    /// A decrypted game folder the user dumped: eboot.bin, sce_module/, assets.
    Folder(PathBuf),
}

impl Input {
    /// Read one file by its `/`-separated path inside a package or folder.
    /// `Ok(None)` means there is no such file.
    fn read_file(&self, path: &str, progress: Progress) -> Result<Option<Vec<u8>>> {
        match self {
            Input::Binary(_) => Ok(None),
            Input::Package(pfs) => match pfs.find(path)? {
                Some(file) => Ok(Some(read_source(&file, path, progress)?)),
                None => Ok(None),
            },
            Input::Folder(root) => {
                let full = root.join(safe_relative(path)?);
                if !full.is_file() {
                    return Ok(None);
                }
                let data = fs::read(&full)?;
                progress(data.len() as u64, data.len() as u64, path);
                Ok(Some(data))
            }
        }
    }

    /// Names of the files directly inside `dir`, sorted.
    fn files_in(&self, dir: &str) -> Result<Vec<String>> {
        let mut names = match self {
            Input::Binary(_) => Vec::new(),
            Input::Package(pfs) => match pfs.resolve(dir)? {
                Some(inode) if inode.is_dir() => pfs
                    .iter_dir(&inode)?
                    .into_iter()
                    .filter(|e| e.is_file())
                    .map(|e| e.name)
                    .collect(),
                _ => Vec::new(),
            },
            Input::Folder(root) => {
                let full = root.join(safe_relative(dir)?);
                if !full.is_dir() {
                    return Ok(Vec::new());
                }
                let mut names = Vec::new();
                for entry in fs::read_dir(full)? {
                    let entry = entry?;
                    if entry.file_type()?.is_file() {
                        names.push(entry.file_name().to_string_lossy().into_owned());
                    }
                }
                names
            }
        };
        names.sort();
        Ok(names)
    }

    fn sniff(&self, path: &str) -> Result<bool> {
        let head = match self {
            Input::Binary(_) => return Ok(false),
            Input::Package(pfs) => {
                let Some(file) = pfs.find(path)? else { return Ok(false) };
                let n = file.size().min(8) as usize;
                match file.read_at(0, n) {
                    Ok(h) => h,
                    Err(_) => return Ok(false),
                }
            }
            Input::Folder(root) => {
                use std::io::Read;
                let Ok(f) = fs::File::open(root.join(safe_relative(path)?)) else { return Ok(false) };
                let mut head = Vec::new();
                f.take(8).read_to_end(&mut head)?;
                head
            }
        };
        Ok(is_elf(&head) || is_self(&head))
    }
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
    if path.is_dir() {
        return load_folder(path);
    }
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
        let input = Input::Package(pfs);
        let default_elf = first_elf_candidate(&input, &files)?;
        rows.push(("Files".into(), files.iter().filter(|f| !f.is_dir).count().to_string()));
        return Ok(Loaded { rows, input: Some(input), files, default_elf, key_problem: None });
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

/// Open a decrypted game folder. It must have an eboot.bin (or, once the
/// user has decrypted it themselves, an eboot.elf) at the top.
fn load_folder(root: &Path) -> Result<Loaded> {
    if !MAIN_EXECUTABLES.iter().any(|e| root.join(e).is_file()) {
        return Err(Error::Conversion(format!(
            "{} has no eboot.bin or eboot.elf; pick the game's top folder (the one holding eboot and sce_sys)",
            root.display()
        )));
    }
    let files = walk_folder(root)?;
    let input = Input::Folder(root.to_path_buf());
    let modules = input.files_in(MODULE_DIR)?.len();
    let rows = vec![
        ("Format".to_string(), "Game folder".to_string()),
        ("Files".to_string(), files.iter().filter(|f| !f.is_dir).count().to_string()),
        ("Size".to_string(), fmt_size(files.iter().map(|f| f.size).sum())),
        ("Modules".to_string(), format!("{modules} in {MODULE_DIR}/")),
    ];
    // A dump the user decrypted may have eboot.elf instead of eboot.bin.
    let default_elf = match first_elf_candidate(&input, &files)? {
        Some(p) if p == MAIN_EXECUTABLES[0] => Some(p),
        _ if input.sniff(MAIN_EXECUTABLES[1])? => Some(MAIN_EXECUTABLES[1].to_string()),
        other => other,
    };
    Ok(Loaded { rows, input: Some(input), files, default_elf, key_problem: None })
}

/// Every entry under `root`, depth first and sorted, with `/`-separated paths.
/// Symbolic links are skipped so nothing outside the folder is read.
pub(crate) fn walk_folder(root: &Path) -> Result<Vec<FileRow>> {
    fn visit(dir: &Path, prefix: &str, depth: usize, out: &mut Vec<FileRow>) -> Result<()> {
        if depth > 64 {
            return Ok(());
        }
        let mut entries: Vec<fs::DirEntry> = fs::read_dir(dir)?.collect::<std::io::Result<_>>()?;
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let kind = e.file_type()?;
            let name = e.file_name().to_string_lossy().into_owned();
            let path = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
            if kind.is_dir() {
                out.push(FileRow { path: path.clone(), size: 0, is_dir: true });
                visit(&e.path(), &path, depth + 1, out)?;
            } else if kind.is_file() {
                out.push(FileRow { path, size: e.metadata()?.len(), is_dir: false });
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    visit(root, "", 0, &mut out)?;
    Ok(out)
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

fn first_elf_candidate(input: &Input, files: &[FileRow]) -> Result<Option<String>> {
    for name in ELF_CANDIDATES {
        if input.sniff(name)? {
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
        if input.sniff(&f.path)? {
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
                    first_elf_candidate(input, &files)?.ok_or_else(|| {
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
        Input::Folder(root) => {
            let path = match inner_path {
                Some(p) => p.to_string(),
                None => first_elf_candidate(input, &walk_folder(root)?)?.ok_or_else(|| {
                    Error::Conversion("no ELF or SELF file was found in the folder; pick one from the list".into())
                })?,
            };
            let data = input
                .read_file(&path, progress)?
                .ok_or_else(|| Error::Conversion(format!("{path} was not found in the folder")))?;
            (path, data)
        }
    };
    if !(is_elf(&data) || is_self(&data)) {
        return Err(Error::Conversion(format!("{source} is not an ELF or SELF file")));
    }
    let conversion = to_elf(&data)?;
    write_whole(output, &conversion.data)?;
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

/// Write `data` to `path`, removing the file again if the write fails so a
/// partial file never looks complete.
fn write_whole(path: &Path, data: &[u8]) -> Result<()> {
    let result = fs::write(path, data);
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    Ok(result?)
}

// --------------------------------------------------------------------------
// Export for AnyPS5

/// AnyPS5 looks for a game's modules in this folder beside its executable.
pub const MODULE_DIR: &str = "sce_module";
/// The main executable, best first. A dump the user decrypted may only have eboot.elf.
const MAIN_EXECUTABLES: [&str; 2] = ["eboot.bin", "eboot.elf"];
pub const HOW_TO_RUN: &str = "HOW-TO-RUN.txt";

pub struct ExportReport {
    pub out_dir: PathBuf,
    /// Header of the converted main executable (`source/eboot.elf`).
    pub eboot: crate::elf::ElfInfo,
    /// Modules written to `source/sce_module/`, by file name.
    pub modules: Vec<String>,
    /// Modules that could not be converted: (file name, reason).
    pub skipped: Vec<(String, String)>,
    /// Original files copied into `app0/`.
    pub app0_files: usize,
    pub warnings: Vec<String>,
}

/// Prefix an error's message with the file it is about, keeping its kind.
fn about(name: &str, e: Error) -> Error {
    let m = |msg: String| format!("{name}: {msg}");
    match e {
        Error::Truncated(s) => Error::Truncated(m(s)),
        Error::NotAPackage(s) => Error::NotAPackage(m(s)),
        Error::Corrupt(s) => Error::Corrupt(m(s)),
        Error::Unsupported(s) => Error::Unsupported(m(s)),
        Error::KeyRequired(s) => Error::KeyRequired(m(s)),
        Error::WrongKey(s) => Error::WrongKey(m(s)),
        Error::Conversion(s) => Error::Conversion(m(s)),
        Error::Io(err) => Error::Io(std::io::Error::new(err.kind(), m(err.to_string()))),
    }
}

fn convert_module(data: &[u8]) -> Result<Conversion> {
    if !(is_elf(data) || is_self(data)) {
        return Err(Error::Conversion("not an ELF or SELF file".into()));
    }
    to_elf(data)
}

fn note_conversion(name: &str, c: &Conversion, warnings: &mut Vec<String>) {
    warnings.extend(c.warnings.iter().map(|w| format!("{name}: {w}")));
    warnings.extend(alignment_problems(&c.info).into_iter().map(|w| format!("{name}: {w}")));
}

/// Lay out a package or game folder the way AnyPS5 expects it:
///
/// ```text
/// out_dir/source/eboot.elf             eboot.bin converted to ELF
/// out_dir/source/sce_module/<name>     each module in sce_module/ converted to ELF
/// out_dir/app0/...                     every original file
/// out_dir/HOW-TO-RUN.txt
/// ```
///
/// A module that cannot be converted is reported in `skipped` and not
/// written; the export carries on. The main executable must convert, or
/// nothing is written.
pub fn export_anyps5(input: &Input, out_dir: &Path, progress: Progress) -> Result<ExportReport> {
    if let Input::Binary(_) = input {
        return Err(Error::Conversion(
            "Export for AnyPS5 needs a package or a game folder, not a single executable".into(),
        ));
    }

    let mut found = None;
    for name in MAIN_EXECUTABLES {
        if let Some(data) = input.read_file(name, progress)? {
            found = Some((name, data));
            break;
        }
    }
    let (eboot_name, eboot_data) =
        found.ok_or_else(|| Error::Conversion("there is no eboot.bin or eboot.elf at the top of the input".into()))?;
    let eboot = convert_module(&eboot_data).map_err(|e| about(eboot_name, e))?;
    drop(eboot_data);

    let existed = out_dir.exists();
    fs::create_dir_all(out_dir)?;
    if let Input::Folder(root) = input {
        let (root, out) = (fs::canonicalize(root)?, fs::canonicalize(out_dir)?);
        if out.starts_with(&root) {
            if !existed {
                let _ = fs::remove_dir(out_dir);
            }
            return Err(Error::Conversion(
                "pick an export folder outside the game folder; it would otherwise copy itself".into(),
            ));
        }
    }

    let mut warnings = Vec::new();
    note_conversion(eboot_name, &eboot, &mut warnings);
    let source = out_dir.join("source");
    let modules_out = source.join(MODULE_DIR);
    fs::create_dir_all(&modules_out)?;
    write_whole(&source.join("eboot.elf"), &eboot.data)?;

    let (mut modules, mut skipped) = (Vec::new(), Vec::new());
    for name in input.files_in(MODULE_DIR)? {
        let dest = modules_out.join(safe_relative(&name)?);
        let result = (|| -> Result<Conversion> {
            let rel = format!("{MODULE_DIR}/{name}");
            let data = input
                .read_file(&rel, progress)?
                .ok_or_else(|| Error::Conversion(format!("{rel} could not be opened")))?;
            let c = convert_module(&data)?;
            write_whole(&dest, &c.data)?;
            Ok(c)
        })();
        match result {
            Ok(c) => {
                note_conversion(&format!("{MODULE_DIR}/{name}"), &c, &mut warnings);
                modules.push(name);
            }
            Err(e) => {
                // Don't leave a file from an earlier export behind.
                let _ = fs::remove_file(&dest);
                skipped.push((name, e.to_string()));
            }
        }
    }

    let app0 = out_dir.join("app0");
    let app0_files = match input {
        Input::Package(pfs) => extract_all(pfs, &app0, progress)?,
        Input::Folder(root) => copy_folder(root, &app0, progress)?,
        Input::Binary(_) => unreachable!(),
    };
    if !app0.join("sce_sys").join("param.json").is_file() {
        warnings.push("sce_sys/param.json was not found in the input (PS5 titles normally have one)".into());
    }

    let report = ExportReport {
        out_dir: out_dir.to_path_buf(),
        eboot: eboot.info,
        modules,
        skipped,
        app0_files,
        warnings,
    };
    write_whole(&out_dir.join(HOW_TO_RUN), how_to_run(&report).as_bytes())?;
    Ok(report)
}

/// Copy every file under `root` into `dest`. Returns the file count.
pub(crate) fn copy_folder(root: &Path, dest: &Path, progress: Progress) -> Result<usize> {
    let entries = walk_folder(root)?;
    let total: u64 = entries.iter().filter(|f| !f.is_dir).map(|f| f.size).sum();
    let mut done = 0u64;
    let mut written = 0usize;
    fs::create_dir_all(dest)?;
    for f in &entries {
        let rel = safe_relative(&f.path)?;
        let to = dest.join(&rel);
        if f.is_dir {
            fs::create_dir_all(&to)?;
            continue;
        }
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        if let Err(e) = fs::copy(root.join(&rel), &to) {
            let _ = fs::remove_file(&to);
            return Err(e.into());
        }
        done += f.size;
        written += 1;
        progress(done, total, &f.path);
    }
    progress(total, total, "done");
    Ok(written)
}

fn how_to_run(r: &ExportReport) -> String {
    let mut s = String::new();
    s.push_str("Exported by PKG Master for AnyPS5 (https://github.com/boykopovar/AnyPS5).\n");
    s.push_str("AnyPS5 is not included here; get it from its own project page.\n\n");
    s.push_str("What is in this folder\n");
    s.push_str("  source/eboot.elf      the main executable, converted from eboot.bin\n");
    s.push_str(&format!("  source/sce_module/    {} module(s) converted to ELF\n", r.modules.len()));
    s.push_str(&format!("  app0/                 the {} original file(s)\n\n", r.app0_files));
    s.push_str("Next steps, run from this folder\n");
    s.push_str("  1. relinker --windows source/eboot.elf app.exe\n");
    s.push_str("  2. Copy AnyPS5's libs/*.prx next to app.exe.\n");
    if !r.skipped.is_empty() {
        s.push_str("\nModules that were not converted (AnyPS5 will be missing them)\n");
        for (name, why) in &r.skipped {
            s.push_str(&format!("  {name}: {why}\n"));
        }
    }
    if !r.warnings.is_empty() {
        s.push_str("\nNotes\n");
        for w in &r.warnings {
            s.push_str(&format!("  {w}\n"));
        }
    }
    s
}
