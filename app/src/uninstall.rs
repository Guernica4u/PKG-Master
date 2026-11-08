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

/// On Windows the running program can't delete itself: a hidden PowerShell
/// waits for it to exit, deletes what is left (and shortcuts to the
/// program), removes the emptied folders, then deletes its own script.
#[cfg(windows)]
pub fn finish_after_exit(leftovers: &[PathBuf], dirs: &[PathBuf]) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    let quote = |p: &Path| format!("'{}'", p.display().to_string().replace('\'', "''"));
    let list = |ps: &[PathBuf]| ps.iter().map(|p| quote(p)).collect::<Vec<_>>().join(",");
    let exe = crate::update::program_path()?;
    let script = std::env::temp_dir().join(format!("pkgmaster-uninstall-{}.ps1", std::process::id()));
    let body = format!(
        "$ErrorActionPreference = 'SilentlyContinue'\r\n\
         Wait-Process -Id {pid} -Timeout 60\r\n\
         foreach ($p in @({leftovers})) {{ Remove-Item -LiteralPath $p -Recurse -Force }}\r\n\
         $sh = New-Object -ComObject WScript.Shell\r\n\
         foreach ($d in @([Environment]::GetFolderPath('Desktop'), [Environment]::GetFolderPath('Programs'))) {{\r\n\
           Get-ChildItem -LiteralPath $d -Filter *.lnk -Recurse | Where-Object {{ $sh.CreateShortcut($_.FullName).TargetPath -eq {exe} }} | Remove-Item -Force\r\n\
         }}\r\n\
         foreach ($d in @({dirs})) {{ if (-not (Get-ChildItem -LiteralPath $d -Force)) {{ Remove-Item -LiteralPath $d -Force }} }}\r\n\
         Remove-Item -LiteralPath $PSCommandPath -Force\r\n",
        pid = std::process::id(),
        leftovers = list(leftovers),
        exe = quote(&exe),
        dirs = list(dirs),
    );
    fs::write(&script, body)?;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-File"])
        .arg(&script)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()?;
    Ok(())
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
}
