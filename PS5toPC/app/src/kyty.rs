//! KytyPS5, the PS5 emulator: "Download for me" fetches its official build
//! from GitHub, and Run starts `kyty_emulator --game <folder>`. KytyPS5 isn't
//! shipped with PS5toPC.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use crate::convert::Progress;
use crate::download::{download, latest_release, untar, unzip};
use crate::error::{Error, Result};

pub const KYTY_REPO: &str = "KytyPS5/KytyPS5";
pub const KYTY_PAGE: &str = "https://github.com/KytyPS5/KytyPS5/releases";
#[cfg(windows)]
pub const KYTY_EXE: &str = "kyty_emulator.exe";
#[cfg(not(windows))]
pub const KYTY_EXE: &str = "kyty_emulator";
#[cfg(windows)]
const ASSET_SUFFIX: &str = "-windows-x64.zip";
#[cfg(not(windows))]
const ASSET_SUFFIX: &str = "-linux-x86_64.tar.gz";

/// Whether `name` is this platform's KytyPS5 download.
fn is_build(name: &str) -> bool {
    name.to_lowercase().ends_with(ASSET_SUFFIX)
}

/// Download the latest official KytyPS5 build and unpack it into `dest`.
/// Returns the emulator to start. A folder this created is removed again if
/// it fails or is cancelled.
pub fn install(dest: &Path, progress: Progress, cancel: &AtomicBool) -> Result<PathBuf> {
    let release = latest_release(KYTY_REPO)?;
    let asset = release.assets.iter().find(|a| is_build(&a.name)).cloned().ok_or_else(|| {
        Error::Conversion(format!(
            "KytyPS5's latest build ({}) has no download for this system; get it by hand from {KYTY_PAGE}",
            release.tag
        ))
    })?;
    let fresh = !dest.exists();
    let result = (|| {
        fs::create_dir_all(dest)?;
        let archive = dest.join(&asset.name);
        download(&asset, &archive, progress, cancel)?;
        let unpacked = if cfg!(windows) { unzip(&archive, dest, progress, cancel) } else { untar(&archive, dest) };
        let _ = fs::remove_file(&archive);
        unpacked?;
        let exe = find_exe(dest).ok_or_else(|| Error::Conversion(format!("the KytyPS5 download has no {KYTY_EXE} (in {})", dest.display())))?;
        crate::anyps5::make_executable(&exe)?;
        Ok(exe)
    })();
    if result.is_err() && fresh {
        let _ = fs::remove_dir_all(dest);
    }
    result
}

/// `kyty_emulator(.exe)` in `dir` or one folder below it.
pub fn find_exe(dir: &Path) -> Option<PathBuf> {
    let direct = dir.join(KYTY_EXE);
    if direct.is_file() {
        return Some(direct);
    }
    fs::read_dir(dir).ok()?.filter_map(|e| e.ok()).map(|e| e.path().join(KYTY_EXE)).find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_this_platforms_build() {
        let (win, linux, mac) = (
            "KytyPS5-2026-11-21-6133172-Windows-x64.zip",
            "KytyPS5-2026-11-21-6133172-Linux-x86_64.tar.gz",
            "KytyPS5-2026-11-21-6133172-macOS-x86_64.zip",
        );
        assert_eq!(is_build(win), cfg!(windows));
        assert_eq!(is_build(linux), !cfg!(windows));
        assert!(!is_build(mac));
    }

    /// Downloads the real latest build (~30 MB): `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn downloads_the_real_build() {
        let dir = std::env::temp_dir().join(format!("ps5topc-kyty-dl-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let exe = install(&dir, &|_, _, _| {}, &AtomicBool::new(false)).unwrap();
        assert!(exe.is_file() && exe.ends_with(KYTY_EXE), "{}", exe.display());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn finds_the_emulator_one_folder_down() {
        let dir = std::env::temp_dir().join(format!("ps5topc-kyty-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("KytyPS5")).unwrap();
        assert_eq!(find_exe(&dir), None);
        fs::write(dir.join("KytyPS5").join(KYTY_EXE), b"").unwrap();
        assert_eq!(find_exe(&dir), Some(dir.join("KytyPS5").join(KYTY_EXE)));
        let _ = fs::remove_dir_all(&dir);
    }
}
