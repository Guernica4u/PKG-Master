//! Uninstall: everything PKG Master put on the computer, so it can be shown
//! to the user, confirmed and removed.
//!
//! Always removed: the settings, History (with its backups), the Library,
//! update leftovers, interrupted downloads and the program itself. Offered
//! as unticked choices: the AnyPS5 and RPCS3 folders the app downloaded
//! (they hold game run folders and RPCS3 saves) and a shadpkg folder with
//! the user's copy of shadPKG in it.

use std::fs;
use std::path::{Path, PathBuf};

use crate::anyps5::{local_data_dir, settings_path, Settings};

/// One thing to delete.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub path: PathBuf,
    /// What it is, in plain words.
    pub what: String,
    /// Bytes on disk.
    pub size: u64,
    /// Kept unless the user ticks it.
    pub optional: bool,
}

/// What the app keeps next to its settings file.
const DATA_FILES: [(&str, &str); 4] = [
    ("history.tsv", "History"),
    ("backups", "History backups"),
    ("library.json", "Library"),
    ("library-icons", "Library icons"),
];

/// Everything to offer for removal on this computer.
pub fn plan(settings: &Settings) -> Vec<Item> {
    let program = crate::update::program_path().ok();
    plan_in(&settings_path(), program.as_deref(), local_data_dir().as_deref(), settings.anyps5_dir.as_deref())
}

/// [`plan`] for given locations (so it can be tested).
pub fn plan_in(settings_file: &Path, program: Option<&Path>, local_dir: Option<&Path>, anyps5_dir: Option<&Path>) -> Vec<Item> {
    let mut items = Vec::new();
    let mut add = |path: PathBuf, what: &str, optional: bool| {
        if fs::symlink_metadata(&path).is_ok() && !items.iter().any(|i: &Item| i.path == path) {
            items.push(Item { size: size_of(&path), path, what: what.to_string(), optional });
        }
    };

    add(settings_file.to_path_buf(), "Settings", false);
    if let Some(data) = settings_file.parent() {
        for (name, what) in DATA_FILES {
            add(data.join(name), what, false);
        }
    }

    if let Some(exe) = program {
        add(exe.to_path_buf(), "PKG Master", false);
        add(old(exe), "Update leftover", false);
        if let Some(dir) = exe.parent() {
            for name in companions() {
                add(dir.join(name), "PKG Master", false);
                add(old(&dir.join(name)), "Update leftover", false);
            }
            // .pkgmaster-update and .pkgmaster-write-test-<pid> leftovers.
            for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
                if entry.file_name().to_string_lossy().starts_with(".pkgmaster-") {
                    add(entry.path(), "Update leftover", false);
                }
            }
            let shadpkg = dir.join("shadpkg");
            let empty = fs::read_dir(&shadpkg).map(|mut d| d.next().is_none()).unwrap_or(false);
            add(shadpkg, if empty { "Empty shadpkg folder" } else { "shadpkg folder (your copy of shadPKG)" }, !empty);
            add(dir.join("AnyPS5"), "AnyPS5 download, with your games' run folders", true);
            add(dir.join("rpcs3"), "RPCS3 download, with its settings and saves", true);
        }
    }

    // Update AnyPS5 puts each new release in an AnyPS5-v… folder.
    if let Some(dir) = anyps5_dir.filter(|d| d.file_name().is_some_and(|n| n.to_string_lossy().starts_with("AnyPS5-v"))) {
        add(dir.to_path_buf(), "AnyPS5 download, with your games' run folders", true);
    }

    if let Some(local) = local_dir {
        add(local.join("AnyPS5"), "AnyPS5 download, with your games' run folders", true);
        add(local.join("rpcs3"), "RPCS3 download, with its settings and saves", true);
        // Anything else in the app's own local folder is ours too.
        for entry in fs::read_dir(local).into_iter().flatten().flatten() {
            add(entry.path(), "Downloads left behind", false);
        }
    }
    items
}

#[cfg(windows)]
fn companions() -> [&'static str; 2] {
    ["pkgmaster-cli.exe", "README.txt"]
}
#[cfg(not(windows))]
fn companions() -> [&'static str; 2] {
    ["pkgmaster-cli", "README.txt"]
}

fn old(p: &Path) -> PathBuf {
    PathBuf::from(format!("{}.old", p.display()))
}

/// Bytes used by a file or a whole folder (links are not followed).
pub fn size_of(p: &Path) -> u64 {
    match fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => fs::read_dir(p).into_iter().flatten().flatten().map(|e| size_of(&e.path())).sum(),
        Ok(m) => m.len(),
        Err(_) => 0,
    }
}

/// Delete `paths`, returning the ones that could not be removed yet (on
/// Windows, the running program).
pub fn remove(paths: &[PathBuf]) -> Vec<PathBuf> {
    paths
        .iter()
        .filter(|p| {
            let gone = match fs::symlink_metadata(p) {
                Ok(m) if m.is_dir() => fs::remove_dir_all(p),
                Ok(_) => fs::remove_file(p),
                Err(_) => Ok(()),
            };
            gone.is_err()
        })
        .cloned()
        .collect()
}

/// Remove each folder that is now empty.
pub fn prune(dirs: &[PathBuf]) {
    for d in dirs {
        let _ = fs::remove_dir(d);
    }
}

/// The folders to drop once they are empty: the data folder, the local
/// folder and the program's folder.
pub fn folders() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = settings_path().parent().map(Path::to_path_buf).into_iter().collect();
    dirs.extend(local_data_dir());
    dirs.extend(crate::update::program_path().ok().and_then(|p| p.parent().map(Path::to_path_buf)));
    dirs
}

/// Windows won't let a running program delete itself, and a hidden helper
/// script that does it looks like malware to antivirus. So shortcuts go now,
/// and what is still locked (the program) is shown in Explorer to delete.
#[cfg(windows)]
pub fn finish_after_exit(leftovers: &[PathBuf], _dirs: &[PathBuf]) -> std::io::Result<()> {
    let exe = crate::update::program_path()?;
    for lnk in shortcuts().into_iter().filter(|l| fs::read(l).is_ok_and(|b| lnk_points_to(&b, &exe))) {
        let _ = fs::remove_file(lnk);
    }
    if let Some(first) = leftovers.first() {
        std::process::Command::new("explorer").arg(format!("/select,{}", first.display())).spawn()?;
    }
    Ok(())
}

/// .lnk files on the Desktop and in the Start menu.
/// shortcut: fixed folder names, not the known-folder API; a moved Desktop is missed.
#[cfg(windows)]
fn shortcuts() -> Vec<PathBuf> {
    let env = |k| std::env::var_os(k).map(PathBuf::from);
    let mut dirs: Vec<PathBuf> = Vec::new();
    dirs.extend(env("USERPROFILE").map(|h| h.join("Desktop")));
    dirs.extend(env("OneDrive").map(|h| h.join("Desktop")));
    dirs.extend(env("APPDATA").map(|a| a.join(r"Microsoft\Windows\Start Menu\Programs")));
    let mut found = Vec::new();
    while let Some(d) = dirs.pop() {
        for e in fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                dirs.push(p);
            } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("lnk")) {
                found.push(p);
            }
        }
    }
    found
}

/// Whether a .lnk file's bytes name `exe`: shortcuts store the target path
/// as ANSI (LinkInfo) and/or UTF-16 (string data). Case doesn't matter.
#[cfg_attr(not(windows), allow(dead_code))]
fn lnk_points_to(bytes: &[u8], exe: &Path) -> bool {
    let target = exe.to_string_lossy().to_lowercase();
    let utf16 = |b: &[u8]| {
        let units: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&units).to_lowercase()
    };
    // UTF-16 text can start on an odd byte, so try both alignments.
    String::from_utf8_lossy(bytes).to_lowercase().contains(&target)
        || utf16(bytes).contains(&target)
        || utf16(bytes.get(1..).unwrap_or_default()).contains(&target)
}

/// Linux deletes a running program's file without trouble, so nothing is
/// left to do after exit.
#[cfg(not(windows))]
pub fn finish_after_exit(leftovers: &[PathBuf], dirs: &[PathBuf]) -> std::io::Result<()> {
    let _ = remove(leftovers);
    prune(dirs);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_and_removes_everything_the_app_made() {
        let root = std::env::temp_dir().join(format!("pkgmaster-uninstall-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (data, app, local) = (root.join("data"), root.join("app"), root.join("local"));
        for d in [data.join("backups/1"), data.join("library-icons"), app.join("shadpkg"), app.join("AnyPS5/game-run"), local.join("rpcs3")] {
            fs::create_dir_all(d).unwrap();
        }
        for (f, body) in [
            (data.join("settings.ini"), "x"),
            (data.join("history.tsv"), "x"),
            (data.join("backups/1/old.elf"), "12345"),
            (data.join("library.json"), "[]"),
            (app.join("pkgmaster-gui.exe"), "gui"),
            (app.join("pkgmaster-gui.exe.old"), "old"),
            (app.join("README.txt"), "r"),
            (app.join(".pkgmaster-write-test-123"), ""),
            (app.join("AnyPS5/game-run/app.exe"), "game"),
            (local.join("rpcs3/rpcs3.exe"), "emu"),
            (local.join("half.zip.part"), "part"),
            (root.join("unrelated.txt"), "keep me"),
        ] {
            fs::write(f, body).unwrap();
        }
        let items = plan_in(&data.join("settings.ini"), Some(&app.join("pkgmaster-gui.exe")), Some(&local), None);
        let find = |p: &Path| items.iter().find(|i| i.path == p).cloned();

        // App data and leftovers always go; downloads are optional.
        assert!(!find(&data.join("backups")).unwrap().optional);
        assert_eq!(find(&data.join("backups")).unwrap().size, 5);
        assert!(!find(&app.join("pkgmaster-gui.exe.old")).unwrap().optional);
        assert!(find(&app.join(".pkgmaster-write-test-123")).is_some());
        assert!(!find(&local.join("half.zip.part")).unwrap().optional);
        assert!(find(&app.join("AnyPS5")).unwrap().optional);
        assert!(find(&local.join("rpcs3")).unwrap().optional);
        // An empty shadpkg folder just goes.
        assert!(!find(&app.join("shadpkg")).unwrap().optional);
        // Nothing outside the app's places, and no duplicates.
        assert!(items.iter().all(|i| i.path != root.join("unrelated.txt")));
        assert_eq!(items.len(), items.iter().map(|i| &i.path).collect::<std::collections::HashSet<_>>().len());

        // Remove everything except the optional downloads.
        let chosen: Vec<PathBuf> = items.iter().filter(|i| !i.optional).map(|i| i.path.clone()).collect();
        assert!(remove(&chosen).is_empty());
        prune(&[data.clone(), local.clone(), app.clone()]);
        assert!(!data.exists());
        assert!(app.join("AnyPS5/game-run/app.exe").exists());
        assert!(local.join("rpcs3/rpcs3.exe").exists());
        assert!(!local.join("half.zip.part").exists());
        assert!(root.join("unrelated.txt").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn finds_the_target_in_a_shortcut() {
        let exe = Path::new(r"C:\Games\PKG Master\pkgmaster-gui.exe");
        let wide: Vec<u8> = r"c:\games\pkg master\PKGMASTER-GUI.EXE".encode_utf16().flat_map(u16::to_le_bytes).collect();
        for pad in [0usize, 1] {
            let mut lnk = vec![0x4c; pad + 3];
            lnk.extend(&wide);
            assert!(lnk_points_to(&lnk, exe), "utf-16 at offset {pad}");
        }
        assert!(lnk_points_to(br"..C:\Games\PKG Master\pkgmaster-gui.exe\0", exe));
        assert!(!lnk_points_to(br"..C:\Other\pkgmaster-gui.exe\0", exe));
    }
}
