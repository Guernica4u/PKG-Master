//! Run in AnyPS5: build the relinker's input from a decrypted game folder,
//! run the user's own AnyPS5 relinker on it, and lay out the runtime folder
//! described in AnyPS5's docs/user/USAGE.md:
//!
//! ```text
//! out/source/eboot.elf              main executable as ELF (relinker input)
//! out/source/sce_module/*.prx       modules converted to ELF
//! out/source/Media/Modules/*.prx    Unity modules, when the game has them
//! out/source/Media/Plugins/*.prx
//! out/app.exe                       written by the relinker
//! out/libs/*.prx                    copied from the AnyPS5 release
//! out/app0/...                      every original game file
//! ```
//!
//! AnyPS5 is never bundled or shipped with this app; the user points it at a
//! release folder they got themselves, or asks it to download the official
//! release from AnyPS5's GitHub page.

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};

use crate::convert::{copy_folder, safe_relative, walk_folder, Progress, MODULE_DIR};
use crate::elf::{alignment_problems, is_elf, is_self, to_elf, ElfInfo};
use crate::error::{Error, Result};

/// Line-by-line output from a child process. Called from reader threads.
pub type Log<'a> = &'a (dyn Fn(&str) + Sync);

/// Executables the relinker can start from, best first.
const EBOOTS: [&str; 2] = ["eboot.bin", "eboot.elf"];
/// Unity keeps its own modules here; they are passed with `--module-dir`.
const UNITY_MODULE_DIRS: [&str; 2] = ["Media/Modules", "Media/Plugins"];
const MODULE_EXTS: [&str; 2] = ["prx", "sprx"];
/// First four bytes of a PS4 SELF (`4F 15 3D 1D`).
const PS4_SELF_MAGIC: [u8; 4] = [0x4F, 0x15, 0x3D, 0x1D];
pub const SOURCE_DIR: &str = "source";
pub const APP0_DIR: &str = "app0";
pub const LIBS_DIR: &str = "libs";

/// The relinker is always run with `--windows`.
pub const APP_EXE: &str = "app.exe";
/// Release assets are versioned, e.g. `relinker-v0.1.2.exe`.
const RELINKER_PREFIX: &str = "relinker";
#[cfg(windows)]
const RELINKER_EXT: &str = ".exe";
#[cfg(not(windows))]
const RELINKER_EXT: &str = "";
/// What gets copied into `libs/`: AnyPS5's system libraries and, on Windows,
/// the MinGW runtime DLLs they load (libgcc_s_seh-1.dll and friends).
const LIB_EXTS: [&str; 2] = ["prx", "dll"];

// --------------------------------------------------------------------------
// Settings

/// Where the AnyPS5 choices are remembered: `%APPDATA%\PKG Master\settings.ini`,
/// or beside the executable when there is no APPDATA.
pub fn settings_path() -> PathBuf {
    if let Some(p) = std::env::var_os("PKGMASTER_SETTINGS") {
        return PathBuf::from(p);
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        return Path::new(&appdata).join("PKG Master").join("settings.ini");
    }
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("pkgmaster-settings.ini")))
        .unwrap_or_else(|| PathBuf::from("pkgmaster-settings.ini"))
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    /// AnyPS5's release folder (the one holding libs/).
    pub anyps5_dir: Option<PathBuf>,
    /// The relinker, when it is not inside `anyps5_dir`.
    pub relinker: Option<PathBuf>,
}

impl Settings {
    /// Missing or unreadable settings are the defaults.
    pub fn load(path: &Path) -> Settings {
        let mut s = Settings::default();
        let Ok(text) = fs::read_to_string(path) else { return s };
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim();
            if v.is_empty() {
                continue;
            }
            match k.trim() {
                "anyps5_dir" => s.anyps5_dir = Some(PathBuf::from(v)),
                "relinker" => s.relinker = Some(PathBuf::from(v)),
                _ => {}
            }
        }
        s
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut text = String::from("# PKG Master settings
");
        if let Some(d) = &self.anyps5_dir {
            text.push_str(&format!("anyps5_dir={}
", d.display()));
        }
        if let Some(r) = &self.relinker {
            text.push_str(&format!("relinker={}
", r.display()));
        }
        Ok(fs::write(path, text)?)
    }

    /// The release these settings describe, if it is complete.
    pub fn release(&self) -> Result<Release> {
        let dir = self
            .anyps5_dir
            .as_deref()
            .ok_or_else(|| Error::Conversion("the AnyPS5 folder is not set".into()))?;
        find_release(dir, self.relinker.as_deref())
    }
}

// --------------------------------------------------------------------------
// The user's AnyPS5 release

#[derive(Clone, Debug)]
pub struct Release {
    pub relinker: PathBuf,
    /// Folder holding AnyPS5's system `.prx` libraries.
    pub libs: PathBuf,
}

/// The `libs/` folder of an AnyPS5 release (or of a build tree, which nests
/// it one level deeper: build/core/libs/libs).
pub fn find_libs(dir: &Path) -> Result<PathBuf> {
    for libs in [dir.join(LIBS_DIR), dir.join(LIBS_DIR).join(LIBS_DIR)] {
        if !files_with_ext(&libs, &["prx"]).is_empty() {
            return Ok(libs);
        }
    }
    Err(Error::Conversion(format!(
        "{} has no libs/ folder with .prx files; pick the folder unpacked from AnyPS5's prx-windows archive",
        dir.display()
    )))
}

/// A relinker inside `dir`: `relinker.exe`, or the newest-named `.exe` with
/// "relinker" anywhere in its name, such as `relinker-v0.1.2.exe`.
pub fn find_relinker(dir: &Path) -> Option<PathBuf> {
    let exact = dir.join(format!("{RELINKER_PREFIX}{RELINKER_EXT}"));
    if exact.is_file() {
        return Some(exact);
    }
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path())
        .filter(|p| is_relinker_name(p))
        .collect();
    found.sort();
    found.pop()
}

fn is_relinker_name(p: &Path) -> bool {
    let name = p.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
    // Any name containing "relinker" counts (relinker-v0.1.2.exe,
    // AnyPS5-relinker.exe, ...), so nobody has to rename the download.
    if !name.contains(RELINKER_PREFIX) {
        return false;
    }
    if RELINKER_EXT.is_empty() {
        !name.contains('.') || name.rsplit_once('.').is_some_and(|(_, e)| e.chars().all(|c| c.is_ascii_digit()))
    } else {
        name.ends_with(RELINKER_EXT)
    }
}

/// Check an AnyPS5 release: `libs/` in `dir`, and the relinker given (if it
/// exists) or one found inside `dir`.
pub fn find_release(dir: &Path, relinker: Option<&Path>) -> Result<Release> {
    let libs = find_libs(dir)?;
    let relinker = relinker
        .filter(|r| r.is_file())
        .map(Path::to_path_buf)
        .or_else(|| find_relinker(dir))
        .ok_or_else(|| {
            Error::Conversion(format!(
                "no relinker was found in {}; pick the relinker (relinker-v….exe from AnyPS5's releases)                  or put it in that folder",
                dir.display()
            ))
        })?;
    Ok(Release { relinker, libs })
}

/// AnyPS5's GitHub repository, where "Download for me" gets the release.
pub const ANYPS5_REPO: &str = "boykopovar/AnyPS5";
pub const ANYPS5_RELEASES: &str = "https://github.com/boykopovar/AnyPS5/releases";

/// Only on the user's request: download the latest official AnyPS5 release
/// (`prx-windows-v….zip` and `relinker-v….exe`) into `dest`, unpack it and
/// put the relinker next to `libs/`. Returns the folder holding `libs/`.
pub fn install_release(dest: &Path, progress: crate::convert::Progress) -> Result<PathBuf> {
    use crate::download::{download, latest_release, unzip};

    let release = latest_release(ANYPS5_REPO)?;
    let pick = |prefix: &str, ext: &str| {
        release
            .assets
            .iter()
            .find(|a| {
                let n = a.name.to_lowercase();
                n.starts_with(prefix) && n.ends_with(ext)
            })
            .cloned()
            .ok_or_else(|| {
                Error::Conversion(format!(
                    "AnyPS5's latest release ({}) has no {prefix}…{ext} file; download it by hand from {ANYPS5_RELEASES}",
                    release.tag
                ))
            })
    };
    let (zip_asset, relinker_asset) = (pick("prx-windows", ".zip")?, pick("relinker", ".exe")?);

    fs::create_dir_all(dest)?;
    let zip_path = dest.join(&zip_asset.name);
    download(&zip_asset, &zip_path, progress)?;
    let unpacked = unzip(&zip_path, dest, progress);
    let _ = fs::remove_file(&zip_path);
    unpacked?;

    // The zip may hold libs/ directly or inside one top-level folder.
    let home = if find_libs(dest).is_ok() {
        dest.to_path_buf()
    } else {
        let subs: Vec<PathBuf> = fs::read_dir(dest)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir() && find_libs(p).is_ok())
            .collect();
        match subs.as_slice() {
            [one] => one.clone(),
            _ => return Err(Error::Conversion(format!("the AnyPS5 download has no libs/ folder (in {})", dest.display()))),
        }
    };
    download(&relinker_asset, &home.join(&relinker_asset.name), progress)?;
    Ok(home)
}

/// Whether `path` starts like a PS4 SELF.
pub fn is_ps4_self(path: &Path) -> bool {
    head4(path) == Some(PS4_SELF_MAGIC)
}

fn has_module_ext(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, e)| MODULE_EXTS.contains(&e.to_lowercase().as_str()))
}

/// Files directly in `dir` with one of `exts` (no dot, any case), sorted.
fn files_with_ext(dir: &Path, exts: &[&str]) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| exts.iter().any(|x| e.eq_ignore_ascii_case(x))))
        .collect();
    out.sort();
    out
}

// --------------------------------------------------------------------------
// Checks before starting

/// Read a string value from a PARAM.SFO. `None` if the file is malformed or
/// has no such key.
pub fn sfo_string(d: &[u8], key: &str) -> Option<String> {
    let u16_at = |o: usize| d.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize);
    let u32_at = |o: usize| d.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
    if d.get(..4)? != b"\0PSF" {
        return None;
    }
    let (keys, data, count) = (u32_at(8)?, u32_at(12)?, u32_at(16)?);
    for i in 0..count.min(4096) {
        let e = 0x14 + i * 16;
        let name_at = keys.checked_add(u16_at(e)?)?;
        let name = d.get(name_at..)?;
        let name = &name[..name.iter().position(|&b| b == 0)?];
        if name != key.as_bytes() {
            continue;
        }
        let (len, at) = (u32_at(e + 4)?, data.checked_add(u32_at(e + 12)?)?);
        let v = d.get(at..at.checked_add(len)?)?;
        let v = &v[..v.iter().position(|&b| b == 0).unwrap_or(v.len())];
        return Some(String::from_utf8_lossy(v).into_owned());
    }
    None
}

/// What was found in a game folder before running AnyPS5.
#[derive(Clone, Debug)]
pub struct Preflight {
    pub title_id: Option<String>,
    /// Why the game looks like a PS4 title, if it does.
    pub ps4: Option<String>,
    pub has_param_json: bool,
    /// Unity module folders present, as passed to `--module-dir`.
    pub module_dirs: Vec<String>,
}

impl Preflight {
    pub fn warnings(&self) -> Vec<String> {
        let mut w = Vec::new();
        if let Some(why) = &self.ps4 {
            w.push(format!(
                "This looks like a PS4 game ({why}). AnyPS5 targets PS5 (PPSA) titles, \
                 so a PS4 game is expected to fail. You can still try."
            ));
        }
        if !self.has_param_json {
            w.push(
                "sce_sys/param.json is missing (PS4 dumps only have param.sfo). AnyPS5 reads the title ID \
                 and name from it, so metadata calls may fail. PKG Master will not make one up."
                    .into(),
            );
        }
        w
    }
}

fn head4(path: &Path) -> Option<[u8; 4]> {
    let mut b = [0u8; 4];
    fs::File::open(path).ok()?.read_exact(&mut b).ok()?;
    Some(b)
}

/// The real on-disk name of `rel` under `root`, matching each part without
/// regard to case (dumps differ in `Media` vs `media`).
fn find_dir(root: &Path, rel: &str) -> Option<String> {
    let mut cur = root.to_path_buf();
    let mut found = Vec::new();
    for part in rel.split('/') {
        let name = fs::read_dir(&cur)
            .ok()?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .find(|n| n.eq_ignore_ascii_case(part))?;
        cur.push(&name);
        found.push(name);
    }
    Some(found.join("/"))
}

pub fn preflight(root: &Path) -> Result<Preflight> {
    if !EBOOTS.iter().any(|e| root.join(e).is_file()) {
        return Err(Error::Conversion(format!("{} has no eboot.bin or eboot.elf", root.display())));
    }
    let sys = root.join("sce_sys");
    let title_id = fs::read(sys.join("param.sfo")).ok().and_then(|d| sfo_string(&d, "TITLE_ID"));
    let mut ps4 = title_id
        .as_ref()
        .filter(|t| t.to_uppercase().starts_with("CUSA"))
        .map(|t| format!("title ID {t}"));
    // A PS5 game (it has param.json) can still ship PS4-format modules, so
    // the executables are only checked when there is no param.json.
    let has_param_json = sys.join("param.json").is_file();
    if ps4.is_none() && !has_param_json {
        let mut candidates: Vec<(String, PathBuf)> =
            EBOOTS.iter().map(|e| (e.to_string(), root.join(e))).collect();
        for f in files_with_ext(&root.join(MODULE_DIR), &["prx"]) {
            let name = f.file_name().unwrap_or_default().to_string_lossy().into_owned();
            candidates.push((format!("{MODULE_DIR}/{name}"), f));
        }
        ps4 = candidates
            .into_iter()
            .find(|(_, p)| head4(p) == Some(PS4_SELF_MAGIC))
            .map(|(name, _)| format!("{name} is a PS4 SELF"));
    }
    let module_dirs = UNITY_MODULE_DIRS.iter().filter_map(|d| find_dir(root, d)).collect();
    Ok(Preflight { title_id, ps4, has_param_json, module_dirs })
}

/// Arguments for the relinker, run from the output folder.
pub fn relinker_args(pre: &Preflight) -> Vec<String> {
    let mut args = vec!["--windows".to_string()];
    for d in &pre.module_dirs {
        args.push("--module-dir".into());
        args.push(d.clone());
    }
    args.push(format!("{SOURCE_DIR}/eboot.elf"));
    args.push(APP_EXE.into());
    args
}

// --------------------------------------------------------------------------
// The relinker's input

#[derive(Debug)]
pub struct SourceReport {
    /// Which file became `source/eboot.elf`.
    pub eboot_from: String,
    pub eboot: ElfInfo,
    /// Modules written under `source/`, as `/`-separated paths.
    pub modules: Vec<String>,
    /// Modules that could not be converted: (path, reason).
    pub skipped: Vec<(String, String)>,
    pub warnings: Vec<String>,
}

fn convert(data: &[u8]) -> Result<crate::elf::Conversion> {
    if !(is_elf(data) || is_self(data)) {
        return Err(Error::Conversion("not an ELF or SELF file".into()));
    }
    to_elf(data)
}

fn note(name: &str, c: &crate::elf::Conversion, warnings: &mut Vec<String>) {
    warnings.extend(c.warnings.iter().map(|w| format!("{name}: {w}")));
    warnings.extend(alignment_problems(&c.info).into_iter().map(|w| format!("{name}: {w}")));
}

/// Write the relinker's input into `source`: `eboot.elf` plus every module in
/// `sce_module/` and the Unity module folders, converted to ELF with their
/// relative paths kept. Nothing in `root` is changed.
pub fn prepare_source(root: &Path, source: &Path, module_dirs: &[String], progress: Progress) -> Result<SourceReport> {
    let mut warnings = Vec::new();
    let mut eboot = None;
    let mut errors = Vec::new();
    for name in EBOOTS {
        let Ok(data) = fs::read(root.join(name)) else { continue };
        progress(0, 0, name);
        match convert(&data) {
            Ok(c) => {
                eboot = Some((name.to_string(), c));
                break;
            }
            Err(e) => errors.push(format!("{name}: {e}")),
        }
    }
    let Some((eboot_from, eboot)) = eboot else {
        return Err(Error::Conversion(if errors.is_empty() {
            "there is no eboot.bin or eboot.elf at the top of the game folder".into()
        } else {
            errors.join("; ")
        }));
    };
    note(&eboot_from, &eboot, &mut warnings);
    fs::create_dir_all(source)?;
    write_whole(&source.join("eboot.elf"), &eboot.data)?;

    let (mut modules, mut skipped) = (Vec::new(), Vec::new());
    let mut dirs = vec![MODULE_DIR.to_string()];
    dirs.extend(module_dirs.iter().cloned());
    for dir in dirs {
        let full = root.join(safe_relative(&dir)?);
        if !full.is_dir() {
            continue;
        }
        for f in walk_folder(&full)? {
            if f.is_dir || !has_module_ext(&f.path) {
                continue;
            }
            let rel = format!("{dir}/{}", f.path);
            let dest = source.join(safe_relative(&rel)?);
            progress(0, 0, &rel);
            let result = (|| -> Result<crate::elf::Conversion> {
                let c = convert(&fs::read(full.join(safe_relative(&f.path)?))?)?;
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent)?;
                }
                write_whole(&dest, &c.data)?;
                Ok(c)
            })();
            match result {
                Ok(c) => {
                    note(&rel, &c, &mut warnings);
                    modules.push(rel);
                }
                Err(e) => {
                    let _ = fs::remove_file(&dest);
                    skipped.push((rel, e.to_string()));
                }
            }
        }
    }
    Ok(SourceReport { eboot_from, eboot: eboot.info, modules, skipped, warnings })
}

fn write_whole(path: &Path, data: &[u8]) -> Result<()> {
    let result = fs::write(path, data);
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    Ok(result?)
}

// --------------------------------------------------------------------------
// Running

pub struct RunReport {
    pub out_dir: PathBuf,
    pub source: SourceReport,
    pub libs: usize,
    pub app0_files: usize,
    pub args: Vec<String>,
}

/// Lay out `out_dir` and run the relinker in it, streaming its output to `log`.
/// Fails if the relinker exits with an error or writes no app.exe.
pub fn run_relinker(
    root: &Path,
    release: &Release,
    out_dir: &Path,
    progress: Progress,
    log: Log,
) -> Result<RunReport> {
    let pre = preflight(root)?;
    let existed = out_dir.exists();
    fs::create_dir_all(out_dir)?;
    let (root_c, out_c) = (fs::canonicalize(root)?, fs::canonicalize(out_dir)?);
    if out_c.starts_with(&root_c) {
        if !existed {
            let _ = fs::remove_dir(out_dir);
        }
        return Err(Error::Conversion(
            "pick an output folder outside the game folder; it would otherwise copy itself".into(),
        ));
    }

    log(&format!("Copying the game into {APP0_DIR}/…"));
    let app0_files = copy_folder(root, &out_dir.join(APP0_DIR), progress)?;

    log(&format!("Converting executables into {SOURCE_DIR}/…"));
    let source = prepare_source(root, &out_dir.join(SOURCE_DIR), &pre.module_dirs, progress)?;
    log(&format!("  eboot.elf from {}", source.eboot_from));
    for m in &source.modules {
        log(&format!("  {m}"));
    }
    for (m, why) in &source.skipped {
        log(&format!("  skipped {m}: {why}"));
    }
    for w in &source.warnings {
        log(&format!("  note: {w}"));
    }

    let libs_out = out_dir.join(LIBS_DIR);
    fs::create_dir_all(&libs_out)?;
    let libs = files_with_ext(&release.libs, &LIB_EXTS);
    for lib in &libs {
        fs::copy(lib, libs_out.join(lib.file_name().unwrap_or_default()))?;
    }
    log(&format!("Copied {} file(s) from AnyPS5's libs into {LIBS_DIR}/", libs.len()));

    let app = out_dir.join(APP_EXE);
    let _ = fs::remove_file(&app);
    let args = relinker_args(&pre);
    log(&format!("> {} {}", release.relinker.display(), args.join(" ")));
    let mut cmd = Command::new(&release.relinker);
    cmd.args(&args).current_dir(out_dir);
    let status = run_logged(cmd, log)?;
    if !status.success() {
        return Err(Error::Conversion(format!("the relinker failed ({status}); see the log")));
    }
    if !app.is_file() {
        return Err(Error::Conversion(format!("the relinker finished but wrote no {APP_EXE}")));
    }
    Ok(RunReport { out_dir: out_dir.to_path_buf(), source, libs: libs.len(), app0_files, args })
}

/// Start `cmd` with its output piped and no console window.
pub fn spawn_piped(mut cmd: Command) -> Result<Child> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    Ok(cmd.spawn()?)
}

/// Send each line of `r` to `log`, decoding bytes that aren't UTF-8 lossily.
pub fn stream_lines(r: impl Read, log: Log) {
    let mut r = BufReader::new(r);
    let mut line = Vec::new();
    loop {
        line.clear();
        match r.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let text = String::from_utf8_lossy(&line);
                let text = text.trim_end_matches(['\r', '\n']);
                // A progress bar redrawn with \r is logged in its final state.
                log(text.rsplit('\r').next().unwrap_or(text));
            }
        }
    }
}

/// Run `cmd` to completion, streaming stdout and stderr to `log`.
pub fn run_logged(cmd: Command, log: Log) -> Result<ExitStatus> {
    let mut child = spawn_piped(cmd)?;
    let (out, err) = (child.stdout.take(), child.stderr.take());
    std::thread::scope(|s| {
        if let Some(o) = out {
            s.spawn(move || stream_lines(o, log));
        }
        if let Some(e) = err {
            s.spawn(move || stream_lines(e, log));
        }
    });
    Ok(child.wait()?)
}

/// The command that starts a laid-out game: `out/app.exe`, run from `out`.
pub fn launch_command(out_dir: &Path) -> Result<Command> {
    let app = out_dir.join(APP_EXE);
    if !app.is_file() {
        return Err(Error::Conversion(format!("{} has no {APP_EXE}; run the relinker first", out_dir.display())));
    }
    let mut cmd = Command::new(app);
    cmd.current_dir(out_dir);
    Ok(cmd)
}
