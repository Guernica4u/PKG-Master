//! Update check and "Update now": find a newer PKG Master release on GitHub,
//! download its zip (checked against GitHub's SHA-256) and swap the new
//! programs in next to the running one.
//!
//! Windows can't overwrite a running .exe but can rename it, so each old
//! file is renamed to `<name>.old` and the new one moved into its place
//! (Linux does the same). The `.old` files are deleted the next time the app
//! starts. On Linux the download is a .tar.gz, or an AppImage when the app
//! runs as one.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use crate::convert::Progress;
use crate::download::{download, latest_release, unzip, Asset, Release};
use crate::error::{Error, Result};

pub const REPO: &str = "Guernica4u/PKG-Master";
pub const RELEASES: &str = "https://github.com/Guernica4u/PKG-Master/releases";

/// The files a release replaces, besides the running program itself.
#[cfg(windows)]
const COMPANIONS: [&str; 2] = ["pkgmaster-cli.exe", "README.txt"];
#[cfg(not(windows))]
const COMPANIONS: [&str; 2] = ["pkgmaster-cli", "README.txt"];
#[cfg(windows)]
const GUI_EXE: &str = "pkgmaster-gui.exe";
#[cfg(not(windows))]
const GUI_EXE: &str = "pkgmaster-gui";
/// How each platform's release download ends.
#[cfg(windows)]
const ARCHIVE_SUFFIX: &str = "-windows-x64.zip";
#[cfg(not(windows))]
const ARCHIVE_SUFFIX: &str = "-linux-x64.tar.gz";
const APPIMAGE_SUFFIX: &str = "-x86_64.appimage";
const OLD_SUFFIX: &str = ".old";

/// The file the user started: the AppImage when running from one (its
/// `current_exe` is inside a temporary mount), otherwise this program.
pub fn program_path() -> std::io::Result<PathBuf> {
    if !cfg!(windows) {
        if let Some(p) = std::env::var_os("APPIMAGE").map(PathBuf::from).filter(|p| p.is_file()) {
            return Ok(p);
        }
    }
    std::env::current_exe()
}

fn running_appimage() -> bool {
    !cfg!(windows) && std::env::var_os("APPIMAGE").is_some()
}

/// The asset "Update now" needs for how this copy was installed.
fn update_asset(release: &Release) -> Option<&Asset> {
    let suffix = if running_appimage() { APPIMAGE_SUFFIX } else { ARCHIVE_SUFFIX };
    release.asset("pkg-master-", suffix)
}

/// This build's version, e.g. `0.1.3`. Debug builds can pretend to be older
/// with `PKGMASTER_VERSION`, to try the updater.
pub fn current_version() -> String {
    #[cfg(debug_assertions)]
    if let Ok(v) = std::env::var("PKGMASTER_VERSION") {
        return v;
    }
    env!("CARGO_PKG_VERSION").to_string()
}

/// `v1.2.3` or `1.2.3` (anything after a `-` is ignored) as numbers.
pub fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let v = v.trim().trim_start_matches(['v', 'V']);
    let v = v.split('-').next()?;
    let mut parts = v.split('.').map(|p| p.parse::<u64>().ok());
    let major = parts.next()??;
    let minor = parts.next().unwrap_or(Some(0))?;
    let patch = parts.next().unwrap_or(Some(0))?;
    Some((major, minor, patch))
}

/// Whether release tag `tag` is newer than version `current`.
pub fn is_newer(tag: &str, current: &str) -> bool {
    match (parse_version(tag), parse_version(current)) {
        (Some(t), Some(c)) => t > c,
        _ => false,
    }
}

/// One line of release notes, made readable without a Markdown renderer.
#[derive(Debug, PartialEq)]
pub enum NoteLine {
    Heading(String),
    Bullet(String),
    Text(String),
}

/// The release notes as plain lines: `###` headings, `-` bullets and text,
/// with `**`, backticks and `[text](link)` reduced to their text. The
/// download instructions and the SHA-256 line are left out, since
/// Update now takes care of both.
pub fn plain_notes(md: &str) -> Vec<NoteLine> {
    let mut out = Vec::new();
    for raw in md.lines() {
        let line = raw.trim();
        let lower = line.to_lowercase();
        if line.is_empty() || lower.starts_with("**download") || lower.starts_with("sha-256") {
            continue;
        }
        let clean = strip_markdown(line.trim_start_matches('#').trim_start_matches(['-', '*']).trim());
        out.push(if line.starts_with('#') {
            NoteLine::Heading(clean)
        } else if line.starts_with("- ") || line.starts_with("* ") {
            NoteLine::Bullet(clean)
        } else {
            NoteLine::Text(clean)
        });
    }
    out
}

fn strip_markdown(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    // [text](link) -> text
    while let Some(open) = rest.find('[') {
        let Some(mid) = rest[open..].find("](").map(|i| open + i) else { break };
        let Some(close) = rest[mid..].find(')').map(|i| mid + i) else { break };
        out.push_str(&rest[..open]);
        out.push_str(&rest[open + 1..mid]);
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out.replace("**", "").replace('`', "").replace('→', "->")
}

/// The latest release, if it is newer than this build and has a download
/// for this platform.
pub fn check() -> Result<Option<Release>> {
    let release = latest_release(REPO)?;
    let newer = is_newer(&release.tag, &current_version()) && update_asset(&release).is_some();
    Ok(newer.then_some(release))
}

/// Download `release` and put it in place of the running program. Returns
/// the path of the new program, to start before this one exits.
pub fn install(release: &Release, progress: Progress, cancel: &AtomicBool) -> Result<PathBuf> {
    let exe = program_path()?;
    let dir = exe.parent().ok_or_else(|| Error::Conversion("can't tell which folder PKG Master is in".into()))?;
    let asset = update_asset(release)
        .ok_or_else(|| Error::Conversion(format!("release {} has no download for this system", release.tag)))?;

    let staging = dir.join(".pkgmaster-update");
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging).map_err(|e| not_writable(dir, e))?;
    let result = (|| {
        let archive = staging.join(&asset.name);
        download(asset, &archive, progress, cancel)?;
        // An AppImage is the whole program: it replaces itself.
        if running_appimage() {
            crate::anyps5::make_executable(&archive)?;
            progress(0, 0, "Replacing the program files");
            swap_in(&[(archive, exe.clone())])?;
            return Ok(exe.clone());
        }
        let unpacked = staging.join("unpacked");
        if cfg!(windows) {
            unzip(&archive, &unpacked, progress, cancel)?;
        } else {
            untar(&archive, &unpacked)?;
        }
        let new_gui = find_file(&unpacked, GUI_EXE)
            .ok_or_else(|| Error::Conversion(format!("the {} download has no {GUI_EXE}", release.tag)))?;
        let new_dir = new_gui.parent().unwrap_or(&unpacked).to_path_buf();

        // The running program keeps its own name, whatever the user called it.
        let mut swaps = vec![(new_gui, exe.clone())];
        for name in COMPANIONS {
            let new = new_dir.join(name);
            if new.is_file() {
                swaps.push((new, dir.join(name)));
            }
        }
        progress(0, 0, "Replacing the program files");
        swap_in(&swaps)?;
        Ok(exe.clone())
    })();
    let _ = fs::remove_dir_all(&staging);
    result
}

/// Unpack a `.tar.gz` with the system's `tar`, which keeps the execute bits.
fn untar(archive: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest)?;
    let status = std::process::Command::new("tar").arg("-xzf").arg(archive).arg("-C").arg(dest).status()?;
    if !status.success() {
        return Err(Error::Conversion(format!("{} could not be unpacked ({status})", archive.display())));
    }
    Ok(())
}

/// Move each new file over its target, keeping the old one as `<target>.old`.
/// If any step fails, every file already swapped is put back.
fn swap_in(swaps: &[(PathBuf, PathBuf)]) -> Result<()> {
    let mut done: Vec<&Path> = Vec::new();
    for (new, target) in swaps {
        let step = (|| -> std::io::Result<()> {
            let old = old_path(target);
            if old.exists() {
                fs::remove_file(&old)?;
            }
            if target.exists() {
                fs::rename(target, &old)?;
            }
            if let Err(e) = fs::rename(new, target).or_else(|_| fs::copy(new, target).map(|_| ())) {
                // Put this one back before rolling back the rest.
                let _ = fs::rename(&old, target);
                return Err(e);
            }
            Ok(())
        })();
        if let Err(e) = step {
            for target in done.iter().rev() {
                let _ = fs::remove_file(target);
                let _ = fs::rename(old_path(target), target);
            }
            return Err(not_writable(target.parent().unwrap_or(Path::new(".")), e));
        }
        done.push(target);
    }
    Ok(())
}

fn old_path(p: &Path) -> PathBuf {
    PathBuf::from(format!("{}{OLD_SUFFIX}", p.display()))
}

fn not_writable(dir: &Path, e: std::io::Error) -> Error {
    Error::Conversion(format!(
        "Could not update the files in {} ({e}). Download the new version from {RELEASES} instead.",
        dir.display()
    ))
}

/// The first file called `name` (any case) in `dir` or one folder below it.
fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    let entries: Vec<PathBuf> = fs::read_dir(dir).ok()?.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    let matches = |p: &Path| p.is_file() && p.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(name));
    entries
        .iter()
        .find(|p| matches(p))
        .cloned()
        .or_else(|| entries.iter().filter(|p| p.is_dir()).find_map(|d| find_file_here(d, &matches)))
}

fn find_file_here(dir: &Path, matches: &dyn Fn(&Path) -> bool) -> Option<PathBuf> {
    fs::read_dir(dir).ok()?.filter_map(|e| e.ok()).map(|e| e.path()).find(|p| matches(p))
}

/// Delete the `.old` files a previous update left next to the program. The
/// old program may still be closing (and locked on Windows), so this keeps
/// trying for a few seconds in the background.
pub fn clean_up_old_files() {
    let Ok(exe) = program_path() else { return };
    let Some(dir) = exe.parent().map(Path::to_path_buf) else { return };
    let mut leftovers = vec![old_path(&exe)];
    leftovers.extend(COMPANIONS.iter().map(|n| old_path(&dir.join(n))));
    let _ = fs::remove_dir_all(dir.join(".pkgmaster-update"));
    std::thread::spawn(move || {
        for _ in 0..20 {
            leftovers.retain(|p| p.exists() && fs::remove_file(p).is_err());
            if leftovers.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert_eq!(parse_version("v0.1.3"), Some((0, 1, 3)));
        assert_eq!(parse_version("1.2"), Some((1, 2, 0)));
        assert_eq!(parse_version("v2.0.0-beta"), Some((2, 0, 0)));
        assert_eq!(parse_version("nightly"), None);
        assert!(is_newer("v0.1.4", "0.1.3"));
        assert!(is_newer("v0.2.0", "0.1.9"));
        assert!(is_newer("v0.1.10", "0.1.9"));
        assert!(!is_newer("v0.1.3", "0.1.3"));
        assert!(!is_newer("v0.1.2", "0.1.3"));
        assert!(!is_newer("junk", "0.1.3"));
    }

    #[test]
    fn notes_lose_their_markdown() {
        let md = "**Download `x.zip`, unzip it.**\n\n### New in v0.1.3\n- **Copy log** next to [Clear](https://x.y/z).\nPlain → text.\n\nSHA-256 of the zip: `abc`";
        assert_eq!(
            plain_notes(md),
            vec![
                NoteLine::Heading("New in v0.1.3".into()),
                NoteLine::Bullet("Copy log next to Clear.".into()),
                NoteLine::Text("Plain -> text.".into()),
            ]
        );
    }

    #[test]
    fn swaps_files_and_keeps_old_ones() {
        let dir = std::env::temp_dir().join(format!("pkgmaster-swap-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("new")).unwrap();
        for (name, old, new) in [("a.exe", "old a", "new a"), ("b.txt", "old b", "new b")] {
            fs::write(dir.join(name), old).unwrap();
            fs::write(dir.join("new").join(name), new).unwrap();
        }
        let swaps: Vec<_> = ["a.exe", "b.txt"].iter().map(|n| (dir.join("new").join(n), dir.join(n))).collect();
        swap_in(&swaps).unwrap();
        assert_eq!(fs::read_to_string(dir.join("a.exe")).unwrap(), "new a");
        assert_eq!(fs::read_to_string(dir.join("a.exe.old")).unwrap(), "old a");
        assert_eq!(fs::read_to_string(dir.join("b.txt")).unwrap(), "new b");

        // A missing new file rolls back everything already swapped.
        fs::write(dir.join("new").join("a.exe"), "newer a").unwrap();
        let bad = vec![(dir.join("new").join("a.exe"), dir.join("a.exe")), (dir.join("new").join("missing"), dir.join("b.txt"))];
        assert!(swap_in(&bad).is_err());
        assert_eq!(fs::read_to_string(dir.join("a.exe")).unwrap(), "new a");
        assert_eq!(fs::read_to_string(dir.join("b.txt")).unwrap(), "new b");
        let _ = fs::remove_dir_all(&dir);
    }
}
