//! RPCS3, the PS3 emulator: "Download for me" fetches its official build
//! (a .7z on Windows, an AppImage on Linux), and a check for the PS3 system
//! software it needs. RPCS3 isn't shipped with PKG Master, and the system
//! software always comes from PlayStation's own site, installed by the user
//! inside RPCS3.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use crate::convert::Progress;
use crate::download::{download, latest_release, un7z};
use crate::error::{Error, Result};

/// Where RPCS3 publishes its official builds for this platform.
#[cfg(windows)]
pub const RPCS3_REPO: &str = "RPCS3/rpcs3-binaries-win";
#[cfg(not(windows))]
pub const RPCS3_REPO: &str = "RPCS3/rpcs3-binaries-linux";
pub const RPCS3_DOWNLOAD: &str = "https://rpcs3.net/download";
/// PlayStation's page for the PS3 system software (PS3UPDAT.PUP), free for anyone.
pub const PS3_SYSTEM_SOFTWARE: &str = "https://www.playstation.com/en-us/support/hardware/ps3/system-software/";
/// The program PKG Master starts: rpcs3.exe, or the AppImage saved under this name.
#[cfg(windows)]
pub const RPCS3_EXE: &str = "rpcs3.exe";
#[cfg(not(windows))]
pub const RPCS3_EXE: &str = "rpcs3.AppImage";

/// Whether `name` is this platform's RPCS3 download.
fn is_build(name: &str) -> bool {
    let n = name.to_lowercase();
    if cfg!(windows) {
        n.ends_with(".7z") && n.contains("win64")
    } else {
        n.ends_with(".appimage") && (n.contains("linux64") || n.contains("x86_64"))
    }
}

/// Download the latest official RPCS3 build into `dest` (unpacking it on
/// Windows). Returns the program to start. A folder this created is removed
/// again if it fails or is cancelled.
pub fn install(dest: &Path, progress: Progress, cancel: &AtomicBool) -> Result<PathBuf> {
    let release = latest_release(RPCS3_REPO)?;
    let asset = release.assets.iter().find(|a| is_build(&a.name)).cloned().ok_or_else(|| {
        Error::Conversion(format!(
            "RPCS3's latest build ({}) has no download for this system; get it by hand from {RPCS3_DOWNLOAD}",
            release.tag
        ))
    })?;
    let fresh = !dest.exists();
    let result = (|| {
        fs::create_dir_all(dest)?;
        if cfg!(windows) {
            let archive = dest.join(&asset.name);
            download(&asset, &archive, progress, cancel)?;
            let unpacked = un7z(&archive, dest, progress, cancel);
            let _ = fs::remove_file(&archive);
            unpacked?;
            find_exe(dest).ok_or_else(|| Error::Conversion(format!("the RPCS3 download has no {RPCS3_EXE} (in {})", dest.display())))
        } else {
            // The AppImage is the whole program.
            let exe = dest.join(RPCS3_EXE);
            download(&asset, &exe, progress, cancel)?;
            crate::anyps5::make_executable(&exe)?;
            Ok(exe)
        }
    })();
    if result.is_err() && fresh {
        let _ = fs::remove_dir_all(dest);
    }
    result
}

/// `rpcs3.exe` in `dir` or one folder below it.
fn find_exe(dir: &Path) -> Option<PathBuf> {
    let direct = dir.join(RPCS3_EXE);
    if direct.is_file() {
        return Some(direct);
    }
    fs::read_dir(dir).ok()?.filter_map(|e| e.ok()).map(|e| e.path().join(RPCS3_EXE)).find(|p| p.is_file())
}

/// Whether the PS3 system software is installed for the RPCS3 at `exe`.
/// RPCS3 unpacks it into `dev_flash`: next to the program on Windows (and in
/// a portable Linux setup), in `~/.config/rpcs3` for a Linux AppImage.
pub fn has_system_software(exe: &Path) -> bool {
    let mut homes: Vec<PathBuf> = exe.parent().map(Path::to_path_buf).into_iter().collect();
    if !cfg!(windows) {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
        homes.extend(config.map(|c| c.join("rpcs3")));
    }
    homes.iter().any(|d| d.join("dev_flash").join("vsh").join("module").join("vsh.self").is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_software_check() {
        let dir = std::env::temp_dir().join(format!("pkgmaster-rpcs3-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("dev_flash/vsh/module")).unwrap();
        let exe = dir.join(RPCS3_EXE);
        fs::write(&exe, b"").unwrap();
        assert!(!has_system_software(&exe));
        fs::write(dir.join("dev_flash/vsh/module/vsh.self"), b"x").unwrap();
        assert!(has_system_software(&exe));
        assert_eq!(find_exe(&dir), Some(exe));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn picks_this_platforms_build() {
        let (win, linux) = ("rpcs3-v0.0.43-20375-0dec46b1_win64_msvc.7z", "rpcs3-v0.0.43-20572-e61d5eaf_linux64.AppImage");
        assert_eq!(is_build(win), cfg!(windows));
        assert_eq!(is_build(linux), !cfg!(windows));
        assert!(!is_build("rpcs3-v0.0.43-20375-0dec46b1_win64_msvc.7z.sha256"));
    }
}
