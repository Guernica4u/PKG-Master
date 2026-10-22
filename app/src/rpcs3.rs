//! RPCS3, the PS3 emulator: "Download for me" fetches its official Windows
//! build, and a check for the PS3 system software it needs. RPCS3 isn't
//! shipped with PKG Master, and the system software always comes from
//! PlayStation's own site, installed by the user inside RPCS3.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use crate::convert::Progress;
use crate::download::{download, latest_release, un7z};
use crate::error::{Error, Result};

/// Where RPCS3 publishes its official Windows builds.
pub const RPCS3_REPO: &str = "RPCS3/rpcs3-binaries-win";
pub const RPCS3_DOWNLOAD: &str = "https://rpcs3.net/download";
/// PlayStation's page for the PS3 system software (PS3UPDAT.PUP), free for anyone.
pub const PS3_SYSTEM_SOFTWARE: &str = "https://www.playstation.com/en-us/support/hardware/ps3/system-software/";
pub const RPCS3_EXE: &str = "rpcs3.exe";

/// Download the latest official RPCS3 build into `dest` and unpack it.
/// Returns the path of `rpcs3.exe`. A folder this created is removed again
/// if it fails or is cancelled.
pub fn install(dest: &Path, progress: Progress, cancel: &AtomicBool) -> Result<PathBuf> {
    let release = latest_release(RPCS3_REPO)?;
    let asset = release
        .assets
        .iter()
        .find(|a| {
            let n = a.name.to_lowercase();
            n.ends_with(".7z") && n.contains("win64")
        })
        .cloned()
        .ok_or_else(|| {
            Error::Conversion(format!(
                "RPCS3's latest build ({}) has no Windows download; get it by hand from {RPCS3_DOWNLOAD}",
                release.tag
            ))
        })?;
    let fresh = !dest.exists();
    let result = (|| {
        fs::create_dir_all(dest)?;
        let archive = dest.join(&asset.name);
        download(&asset, &archive, progress, cancel)?;
        let unpacked = un7z(&archive, dest, progress, cancel);
        let _ = fs::remove_file(&archive);
        unpacked?;
        find_exe(dest).ok_or_else(|| Error::Conversion(format!("the RPCS3 download has no {RPCS3_EXE} (in {})", dest.display())))
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

/// Whether the PS3 system software is installed in the RPCS3 next to `exe`
/// (RPCS3 unpacks it into `dev_flash`).
pub fn has_system_software(exe: &Path) -> bool {
    exe.parent().is_some_and(|d| d.join("dev_flash").join("vsh").join("module").join("vsh.self").is_file())
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
}
