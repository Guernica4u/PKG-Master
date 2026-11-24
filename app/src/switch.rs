//! Switch to PS5toPC, the PS5-only version of ROM Master: download its latest
//! release next to this app and carry the settings and Library over. The
//! window then runs the normal uninstaller on ROM Master.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use crate::anyps5::{app_user_dir, make_executable};
use crate::convert::Progress;
use crate::download::{download, latest_release, unzip};
use crate::error::{Error, Result};
use crate::library::Library;
use crate::update::{find_file, program_path, running_appimage, untar};

pub const REPO: &str = "Guernica4u/PS5toPC";
pub const PAGE: &str = "https://github.com/Guernica4u/PS5toPC";
/// PS5toPC's folder names under %APPDATA% / ~/.config (Windows, Linux).
pub const DIR_NAMES: (&str, &str) = ("PS5toPC", "ps5topc");
#[cfg(windows)]
const GUI_EXE: &str = "ps5topc-gui.exe";
#[cfg(not(windows))]
const GUI_EXE: &str = "ps5topc-gui";

/// Download the latest PS5toPC and unpack it. Returns the program to start.
/// A folder this created is removed again if it fails or is cancelled.
pub fn install(progress: Progress, cancel: &AtomicBool) -> Result<PathBuf> {
    let release = latest_release(REPO)?;
    let appimage = running_appimage();
    let suffix = if appimage {
        "-x86_64.appimage"
    } else if cfg!(windows) {
        "-windows-x64.zip"
    } else {
        "-linux-x64.tar.gz"
    };
    let asset = release
        .asset("ps5topc-", suffix)
        .cloned()
        .ok_or_else(|| Error::Conversion(format!("PS5toPC {} has no download for this system; get it from {PAGE}", release.tag)))?;
    let dest = install_dir();
    let fresh = !dest.exists();
    let result = (|| {
        fs::create_dir_all(&dest)?;
        if appimage {
            let exe = dest.join("PS5toPC.AppImage");
            download(&asset, &exe, progress, cancel)?;
            make_executable(&exe)?;
            return Ok(exe);
        }
        let archive = dest.join(&asset.name);
        download(&asset, &archive, progress, cancel)?;
        let unpacked = if cfg!(windows) { unzip(&archive, &dest, progress, cancel) } else { untar(&archive, &dest) };
        let _ = fs::remove_file(&archive);
        unpacked?;
        let exe = find_file(&dest, GUI_EXE).ok_or_else(|| Error::Conversion(format!("the PS5toPC download has no {GUI_EXE}")))?;
        make_executable(&exe)?;
        Ok(exe)
    })();
    if result.is_err() && fresh {
        let _ = fs::remove_dir_all(&dest);
    }
    result
}

/// A `PS5toPC` folder beside this app's folder, or PS5toPC's own local
/// folder when that can't be written to.
fn install_dir() -> PathBuf {
    let beside = program_path().ok().and_then(|p| p.parent()?.parent().map(|d| d.join("PS5toPC")));
    beside
        .filter(|d| d.parent().is_some_and(can_write))
        .or_else(|| app_user_dir(true, DIR_NAMES).map(|d| d.join("app")))
        .unwrap_or_else(|| PathBuf::from("PS5toPC"))
}

fn can_write(dir: &Path) -> bool {
    let probe = dir.join(format!(".pkgmaster-write-test-{}", std::process::id()));
    let ok = fs::write(&probe, b"").is_ok();
    let _ = fs::remove_file(&probe);
    ok
}

/// PS5toPC's data folder (`%APPDATA%\PS5toPC`, `~/.config/ps5topc`).
pub fn data_dir() -> Option<PathBuf> {
    app_user_dir(false, DIR_NAMES)
}

/// Copy the settings and the Library (with its icons) from ROM Master's data
/// folder to `to`, unless PS5toPC already has settings of its own there.
pub fn carry_over(settings_file: &Path, to: &Path) -> Result<()> {
    if to.join("settings.ini").exists() {
        return Ok(());
    }
    fs::create_dir_all(to)?;
    if settings_file.is_file() {
        fs::copy(settings_file, to.join("settings.ini"))?;
    }
    let Some(from) = settings_file.parent() else { return Ok(()) };
    let old = Library::load(&from.join("library.json"));
    if old.entries.is_empty() {
        return Ok(());
    }
    let (old_icons, new_icons) = (from.join("library-icons"), to.join("library-icons"));
    fs::create_dir_all(&new_icons)?;
    let mut new = Library::load(&to.join("library.json"));
    new.entries = old.entries;
    for e in &mut new.entries {
        // Icons are absolute paths into the old data folder, which the uninstaller deletes.
        if let Some(name) = e.icon.as_ref().filter(|i| i.starts_with(&old_icons)).and_then(|i| i.file_name()) {
            let target = new_icons.join(name);
            e.icon = fs::copy(e.icon.as_ref().unwrap(), &target).ok().map(|_| target);
        }
    }
    new.save()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Downloads the real latest PS5toPC (~4 MB): `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn downloads_the_real_release() {
        let exe = install(&|_, _, _| {}, &AtomicBool::new(false)).unwrap();
        assert!(exe.is_file() && exe.ends_with(GUI_EXE), "{}", exe.display());
        let _ = fs::remove_dir_all(install_dir());
    }

    #[test]
    fn carries_settings_and_library_over() {
        let root = std::env::temp_dir().join(format!("pkgmaster-switch-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (old, new) = (root.join("ROM Master"), root.join("PS5toPC"));
        fs::create_dir_all(old.join("library-icons")).unwrap();
        fs::write(old.join("settings.ini"), "anyps5_dir=C:\\AnyPS5\n").unwrap();
        let icon = old.join("library-icons").join("PPSA01234.png");
        fs::write(&icon, b"png").unwrap();
        let mut lib = Library::load(&old.join("library.json"));
        lib.entries.push(crate::library::Entry {
            path: PathBuf::from(r"D:\games\doom"),
            title: "DOOM".into(),
            title_id: Some("PPSA01234".into()),
            run_dir: None,
            icon: Some(icon),
        });
        lib.save().unwrap();

        carry_over(&old.join("settings.ini"), &new).unwrap();
        assert_eq!(fs::read_to_string(new.join("settings.ini")).unwrap(), "anyps5_dir=C:\\AnyPS5\n");
        let moved = Library::load(&new.join("library.json"));
        assert_eq!(moved.entries[0].title, "DOOM");
        let new_icon = moved.entries[0].icon.clone().unwrap();
        assert!(new_icon.starts_with(&new) && new_icon.is_file());

        // PS5toPC's own settings are never overwritten.
        fs::write(new.join("settings.ini"), "emulator=kyty\n").unwrap();
        carry_over(&old.join("settings.ini"), &new).unwrap();
        assert_eq!(fs::read_to_string(new.join("settings.ini")).unwrap(), "emulator=kyty\n");
        let _ = fs::remove_dir_all(&root);
    }
}
