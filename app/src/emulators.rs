//! The emulators ROM Master can download and start, besides AnyPS5 and RPCS3
//! (which have their own setup): one table, one installer. None of them ship
//! with the app; "Download for me" fetches each project's official build.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use crate::console::Console;
use crate::convert::Progress;
use crate::download::{download, latest_release, un7z, unzip, Asset};
use crate::error::{Error, Result};

/// Where a build comes from.
pub enum Source {
    /// The latest GitHub release, picking the asset by (prefix, suffix) of
    /// its lowercase name: (Windows, Linux). `None` for no build there.
    GitHub(&'static str, Option<(&'static str, &'static str)>, Option<(&'static str, &'static str)>),
    /// Dolphin's own update feed.
    Dolphin,
}

pub struct Emu {
    /// Settings key and download folder name.
    pub id: &'static str,
    pub name: &'static str,
    pub consoles: &'static [Console],
    /// Where people download it by hand.
    pub page: &'static str,
    pub source: Source,
    /// The program inside the download (lowercase; a trailing `*` matches any rest).
    pub exe: &'static [&'static str],
    /// Arguments before the game path.
    pub args: &'static [&'static str],
}

pub const SHADPS4: Emu = Emu {
    id: "shadps4",
    name: "shadPS4",
    consoles: &[],
    page: "https://github.com/shadps4-emu/shadPS4/releases",
    source: Source::GitHub("shadps4-emu/shadPS4", Some(("shadps4-win64-sdl-", ".zip")), Some(("shadps4-linux-sdl-", ".zip"))),
    exe: &["shadps4.exe", "shadps4*.appimage", "shadps4"],
    args: &["-g"],
};

pub const ALL: [Emu; 7] = [
    SHADPS4,
    Emu {
        id: "xenia",
        name: "Xenia Canary",
        consoles: &[Console::Xbox360],
        page: "https://github.com/xenia-canary/xenia-canary-releases/releases",
        source: Source::GitHub(
            "xenia-canary/xenia-canary-releases",
            Some(("xenia_canary_windows", ".zip")),
            Some(("xenia_canary_linux", "")),
        ),
        exe: &["xenia_canary.exe", "xenia_canary"],
        args: &[],
    },
    Emu {
        id: "dolphin",
        name: "Dolphin",
        consoles: &[Console::GameCube, Console::Wii],
        page: "https://dolphin-emu.org/download/",
        source: Source::Dolphin,
        exe: &["dolphin.exe", "dolphin*.appimage"],
        args: &["-b", "-e"],
    },
    Emu {
        id: "pcsx2",
        name: "PCSX2",
        consoles: &[Console::Ps2],
        page: "https://pcsx2.net/downloads",
        source: Source::GitHub("PCSX2/pcsx2", Some(("pcsx2-", "-windows-x64-qt.7z")), Some(("pcsx2-", "-linux-appimage-x64-qt.appimage"))),
        exe: &["pcsx2-qt.exe", "pcsx2*.appimage"],
        args: &["-batch", "--"],
    },
    Emu {
        id: "duckstation",
        name: "DuckStation",
        consoles: &[Console::Ps1],
        page: "https://www.duckstation.org/",
        source: Source::GitHub(
            "stenzek/duckstation",
            Some(("duckstation-windows-x64-release", ".zip")),
            Some(("duckstation-x64.appimage", "")),
        ),
        exe: &["duckstation-qt*.exe", "duckstation*.appimage"],
        args: &["-batch", "--"],
    },
    Emu {
        id: "ppsspp",
        name: "PPSSPP",
        consoles: &[Console::Psp],
        page: "https://www.ppsspp.org/download/",
        source: Source::GitHub("hrydgard/ppsspp", Some(("ppsspp-", "-windows-x64.zip")), Some(("ppsspp-", "-anylinux-x86_64.appimage"))),
        exe: &["ppssppwindows64.exe", "ppsspp*.appimage"],
        args: &[],
    },
    Emu {
        id: "vita3k",
        name: "Vita3K",
        consoles: &[Console::Vita],
        page: "https://vita3k.org/",
        source: Source::GitHub("Vita3K/Vita3K", Some(("windows-latest", ".zip")), Some(("vita3k-x86_64.appimage", ""))),
        exe: &["vita3k.exe", "vita3k*.appimage"],
        args: &[],
    },
];

/// The emulator that runs `console`'s games.
pub fn for_console(console: Console) -> Option<&'static Emu> {
    ALL.iter().find(|e| e.consoles.contains(&console))
}

pub fn by_id(id: &str) -> Option<&'static Emu> {
    ALL.iter().find(|e| e.id == id)
}

impl Emu {
    fn is_exe(&self, name: &str) -> bool {
        let n = name.to_lowercase();
        self.exe.iter().any(|p| match p.split_once('*') {
            Some((start, end)) => n.starts_with(start) && n.ends_with(end),
            None => n == *p,
        })
    }

    /// The program in `dir` or any folder below it.
    pub fn find_exe(&self, dir: &Path) -> Option<PathBuf> {
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for entry in fs::read_dir(&d).ok()?.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p);
                } else if self.is_exe(&entry.file_name().to_string_lossy()) {
                    return Some(p);
                }
            }
        }
        None
    }

    /// This platform's official build.
    fn build(&self) -> Result<Asset> {
        let none = || Error::Conversion(format!("{} has no download for this system; get it from {}", self.name, self.page));
        match &self.source {
            Source::GitHub(repo, win, linux) => {
                let (prefix, suffix) = if cfg!(windows) { win } else { linux }.ok_or_else(none)?;
                latest_release(repo)?.asset(prefix, suffix).cloned().ok_or_else(none)
            }
            Source::Dolphin => {
                let what = |e: String| Error::Conversion(format!("Could not check Dolphin's releases: {e}"));
                let body = ureq::get("https://dolphin-emu.org/update/latest/beta").call().map_err(|e| what(e.to_string()))?.into_string()?;
                let feed: serde_json::Value = serde_json::from_str(&body).map_err(|e| what(e.to_string()))?;
                let system = if cfg!(windows) { "Windows x64" } else { "Linux x64 (AppImage)" };
                let url = feed["artifacts"]
                    .as_array()
                    .and_then(|a| a.iter().find(|x| x["system"] == system))
                    .and_then(|x| x["url"].as_str())
                    .ok_or_else(none)?;
                let name = url.rsplit('/').next().unwrap_or("dolphin").to_string();
                Ok(Asset { name, url: url.to_string(), size: 0, sha256: None })
            }
        }
    }

    /// Download the latest build into `dest` and unpack it. Returns the
    /// program to start. A folder this created is removed if it fails.
    pub fn install(&self, dest: &Path, progress: Progress, cancel: &AtomicBool) -> Result<PathBuf> {
        let asset = self.build()?;
        let fresh = !dest.exists();
        let result = (|| {
            fs::create_dir_all(dest)?;
            let file = dest.join(&asset.name);
            download(&asset, &file, progress, cancel)?;
            let lower = asset.name.to_lowercase();
            if lower.ends_with(".appimage") {
                crate::anyps5::make_executable(&file)?;
                return Ok(file);
            }
            let unpacked = if lower.ends_with(".zip") {
                unzip(&file, dest, progress, cancel)
            } else if lower.ends_with(".7z") {
                un7z(&file, dest, progress, cancel)
            } else {
                // .tar.gz / .tar.xz: the system tar works out the compression.
                std::process::Command::new("tar").arg("-xf").arg(&file).arg("-C").arg(dest).status().map_err(Error::from).and_then(|s| {
                    if s.success() { Ok(()) } else { Err(Error::Conversion(format!("{} could not be unpacked ({s})", asset.name))) }
                })
            };
            let _ = fs::remove_file(&file);
            unpacked?;
            let exe = self
                .find_exe(dest)
                .ok_or_else(|| Error::Conversion(format!("the {} download has no program this app knows ({})", self.name, dest.display())))?;
            crate::anyps5::make_executable(&exe)?;
            Ok(exe)
        })();
        if result.is_err() && fresh {
            let _ = fs::remove_dir_all(dest);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_each_program() {
        let dir = std::env::temp_dir().join(format!("pkgmaster-emus-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for (emu, file) in [
            ("dolphin", "Dolphin-x64/Dolphin.exe"),
            ("duckstation", "duckstation-qt-x64-ReleaseLTCG.exe"),
            ("pcsx2", "pcsx2-qt.exe"),
            ("ppsspp", "PPSSPPWindows64.exe"),
        ] {
            let p = dir.join(emu).join(file);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(dir.join(emu).join("DolphinTool.exe"), b"").unwrap();
            fs::write(&p, b"").unwrap();
            assert_eq!(by_id(emu).unwrap().find_exe(&dir.join(emu)), Some(p));
        }
        assert_eq!(for_console(Console::Wii).unwrap().id, "dolphin");
        assert_eq!(for_console(Console::Xbox360).unwrap().id, "xenia");
        let _ = fs::remove_dir_all(&dir);
    }

    /// Downloads every emulator for real (hundreds of MB): `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn downloads_every_emulator() {
        let dir = std::env::temp_dir().join("pkgmaster-emus-real");
        for emu in &ALL {
            let exe = emu.install(&dir.join(emu.id), &|_, _, _| {}, &AtomicBool::new(false)).unwrap();
            assert!(exe.is_file(), "{}: {}", emu.name, exe.display());
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
