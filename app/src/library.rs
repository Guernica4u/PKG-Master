//! The Library: games the user starred (or ran in AnyPS5), kept in
//! `library.json` next to the settings, with a small copy of each icon in
//! `library-icons/` so the Library can show them without opening the games.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::meta::{GameInfo, Icon};

/// Game folders (holding sce_sys/param.json or param.sfo, or a PS3
/// PS3_GAME/PARAM.SFO) in `dir`, its subfolders and theirs.
pub fn find_games(dir: &Path) -> Vec<PathBuf> {
    let is_game = |p: &Path| {
        p.join("sce_sys/param.json").is_file() || p.join("sce_sys/param.sfo").is_file() || p.join("PS3_GAME/PARAM.SFO").is_file()
    };
    let subdirs = |p: &Path| -> Vec<PathBuf> {
        fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect()
    };
    if is_game(dir) {
        return vec![dir.to_path_buf()];
    }
    let mut games = Vec::new();
    for d in subdirs(dir) {
        if is_game(&d) {
            games.push(d);
        } else {
            games.extend(subdirs(&d).into_iter().filter(|s| is_game(s)));
        }
    }
    games.sort();
    games
}

/// Library icons are stored at most this many pixels wide and high.
const ICON_SIDE: u32 = 128;

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// The game: a .pkg, a game folder or an executable.
    pub path: PathBuf,
    pub title: String,
    pub title_id: Option<String>,
    /// The AnyPS5 run folder (holding app.exe), once the game has one.
    pub run_dir: Option<PathBuf>,
    /// The cached icon PNG.
    pub icon: Option<PathBuf>,
}

impl Entry {
    /// A PS3 game, by its title ID (BLUS, NPEB, ...): those boot in RPCS3.
    pub fn is_ps3(&self) -> bool {
        self.title_id.as_deref().is_some_and(is_ps3_title_id)
    }
}

/// PS3 title IDs start with B (disc) or N (PSN), then a region and type letter.
pub fn is_ps3_title_id(id: &str) -> bool {
    let id = id.to_uppercase();
    id.len() == 9 && (id.starts_with('B') || id.starts_with('N')) && id[4..].chars().all(|c| c.is_ascii_digit())
}

pub struct Library {
    file: PathBuf,
    pub entries: Vec<Entry>,
}

impl Library {
    /// A missing or unreadable library is empty.
    pub fn load(file: &Path) -> Library {
        let entries = fs::read(file)
            .ok()
            .and_then(|d| serde_json::from_slice::<serde_json::Value>(&d).ok())
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|e| {
                let text = |k: &str| e.get(k).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_string);
                Some(Entry {
                    path: PathBuf::from(text("path")?),
                    title: text("title").unwrap_or_default(),
                    title_id: text("title_id"),
                    run_dir: text("run_dir").map(PathBuf::from),
                    icon: text("icon").map(PathBuf::from),
                })
            })
            .collect();
        Library { file: file.to_path_buf(), entries }
    }

    pub fn save(&self) -> Result<()> {
        let list: Vec<serde_json::Value> = self
            .entries
            .iter()
            .map(|e| {
                let path = |p: &Option<PathBuf>| p.as_ref().map(|p| p.display().to_string());
                serde_json::json!({
                    "path": e.path.display().to_string(),
                    "title": e.title,
                    "title_id": e.title_id,
                    "run_dir": path(&e.run_dir),
                    "icon": path(&e.icon),
                })
            })
            .collect();
        if let Some(dir) = self.file.parent() {
            fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(&list).map_err(|e| Error::Conversion(e.to_string()))?;
        Ok(fs::write(&self.file, text)?)
    }

    pub fn get(&self, path: &Path) -> Option<&Entry> {
        self.entries.iter().find(|e| e.path == path)
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.get(path).is_some()
    }

    /// Add (or refresh) the game at `path`, keeping its run folder if it
    /// already had one. New games go to the top.
    pub fn add(&mut self, path: &Path, info: &GameInfo, run_dir: Option<PathBuf>) {
        let old = self.entries.iter().position(|e| e.path == path).map(|i| self.entries.remove(i));
        let icon = info.icon.as_ref().and_then(|i| self.save_icon(path, i)).or(old.as_ref().and_then(|o| o.icon.clone()));
        let title = info.title.clone().unwrap_or_else(|| {
            path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string())
        });
        self.entries.insert(
            0,
            Entry {
                path: path.to_path_buf(),
                title,
                title_id: info.title_id.clone().or(old.as_ref().and_then(|o| o.title_id.clone())),
                run_dir: run_dir.or(old.and_then(|o| o.run_dir)),
                icon,
            },
        );
    }

    /// Add the games in `dir` that aren't in the Library yet. Returns how many.
    pub fn add_folder(&mut self, dir: &Path) -> usize {
        let mut added = 0;
        for game in find_games(dir) {
            if self.contains(&game) {
                continue;
            }
            let read = |name: &str| fs::read(game.join(name)).ok();
            let sfo = read("sce_sys/param.sfo").or_else(|| read("PS3_GAME/PARAM.SFO"));
            let icon = read("sce_sys/icon0.png").or_else(|| read("PS3_GAME/ICON0.PNG"));
            let info = GameInfo::from_files(sfo.as_deref(), read("sce_sys/param.json").as_deref(), icon.as_deref());
            self.add(&game, &info, None);
            added += 1;
        }
        added
    }

    pub fn remove(&mut self, path: &Path) {
        if let Some(i) = self.entries.iter().position(|e| e.path == path) {
            let e = self.entries.remove(i);
            if let Some(icon) = e.icon {
                let _ = fs::remove_file(icon);
            }
        }
    }

    /// Write a small PNG of `icon` into `library-icons/`.
    fn save_icon(&self, path: &Path, icon: &Icon) -> Option<PathBuf> {
        use std::hash::{Hash, Hasher};
        let img = image::RgbaImage::from_raw(icon.width, icon.height, icon.rgba.clone())?;
        let img = if icon.width > ICON_SIDE || icon.height > ICON_SIDE {
            image::imageops::thumbnail(&img, ICON_SIDE, ICON_SIDE)
        } else {
            img
        };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        path.hash(&mut h);
        let dir = self.file.parent()?.join("library-icons");
        fs::create_dir_all(&dir).ok()?;
        let out = dir.join(format!("{:016x}.png", h.finish()));
        img.save_with_format(&out, image::ImageFormat::Png).ok()?;
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(title: &str, id: &str, icon: bool) -> GameInfo {
        GameInfo {
            title: Some(title.into()),
            title_id: Some(id.into()),
            version: None,
            icon: icon.then(|| Icon { width: 256, height: 256, rgba: vec![200; 256 * 256 * 4] }),
        }
    }

    #[test]
    fn add_save_load_remove() {
        let dir = std::env::temp_dir().join(format!("pkgmaster-library-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let file = dir.join("library.json");
        let mut lib = Library::load(&file);
        assert!(lib.entries.is_empty());

        lib.add(Path::new(r"D:\games\doom"), &info("DOOM", "PPSA01234", true), None);
        lib.add(Path::new(r"D:\games\bad.pkg"), &info("Bad North", "CUSA10969", false), None);
        // Re-adding keeps one entry, moves it to the top and keeps the run folder.
        lib.add(Path::new(r"D:\games\doom"), &info("DOOM", "PPSA01234", true), Some(PathBuf::from(r"D:\AnyPS5\doom-run")));
        lib.add(Path::new(r"D:\games\doom"), &info("DOOM", "PPSA01234", true), None);
        assert_eq!(lib.entries.len(), 2);
        assert_eq!(lib.entries[0].title, "DOOM");
        assert_eq!(lib.entries[0].run_dir.as_deref(), Some(Path::new(r"D:\AnyPS5\doom-run")));
        let icon = lib.entries[0].icon.clone().unwrap();
        assert_eq!(image::image_dimensions(&icon).unwrap(), (128, 128));
        lib.save().unwrap();

        let back = Library::load(&file);
        assert_eq!(back.entries, lib.entries);

        lib.remove(Path::new(r"D:\games\doom"));
        assert!(!lib.contains(Path::new(r"D:\games\doom")));
        assert!(!icon.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ps3_title_ids() {
        assert!(is_ps3_title_id("BLUS30443"));
        assert!(is_ps3_title_id("NPEB01234"));
        assert!(!is_ps3_title_id("CUSA10969"));
        assert!(!is_ps3_title_id("PPSA01234"));
        assert!(!is_ps3_title_id("BLUS"));
    }

    #[test]
    fn scans_a_games_folder() {
        let dir = std::env::temp_dir().join(format!("pkgmaster-scan-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for g in ["Game A/sce_sys", "more/Game B/sce_sys", "not a game/stuff", "PS3 game/PS3_GAME"] {
            fs::create_dir_all(dir.join(g)).unwrap();
        }
        fs::write(dir.join("Game A/sce_sys/param.json"), br#"{"titleId":"PPSA00001"}"#).unwrap();
        fs::write(dir.join("more/Game B/sce_sys/param.sfo"), b"").unwrap();
        fs::write(dir.join("PS3 game/PS3_GAME/PARAM.SFO"), b"").unwrap();
        assert_eq!(find_games(&dir), vec![dir.join("Game A"), dir.join("PS3 game"), dir.join("more").join("Game B")]);
        let mut lib = Library::load(&dir.join("library.json"));
        assert_eq!(lib.add_folder(&dir), 3);
        assert_eq!(lib.add_folder(&dir), 0);
        let _ = fs::remove_dir_all(&dir);
    }
}
