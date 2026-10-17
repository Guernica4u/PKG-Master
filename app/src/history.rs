//! A record of everything the app writes to disk, so each change can be
//! reverted.
//!
//! Every operation writes to one target (a file or a folder). Before it runs,
//! an existing target is moved into a backup folder; if the operation fails,
//! its partial output is removed and the backup is put back, so a failed
//! operation changes nothing. A successful one is recorded. Reverting deletes
//! the target and restores the backup, if there was one.
//!
//! The record is a small tab-separated text file next to the settings, and
//! backups live in a `backups` folder beside it. Windows file names cannot
//! contain tabs or newlines, so paths need no escaping.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{Error, Result};

#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub id: u64,
    /// Seconds since the Unix epoch.
    pub time: u64,
    /// What the app did, e.g. "Converted eboot.bin to ELF".
    pub action: String,
    /// The file or folder the operation wrote.
    pub target: PathBuf,
    /// Where the previous contents of `target` were moved, if it existed.
    pub backup: Option<PathBuf>,
    pub reverted: bool,
}

/// An operation in progress: the target has been cleared (moved to backup).
pub struct Pending {
    id: u64,
    action: String,
    target: PathBuf,
    backup: Option<PathBuf>,
}

pub struct History {
    file: PathBuf,
    backups: PathBuf,
    pub changes: Vec<Change>,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Remove a file or a whole folder; a missing path is fine.
fn remove_path(p: &Path) -> Result<()> {
    match fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => Ok(fs::remove_dir_all(p)?),
        Ok(_) => Ok(fs::remove_file(p)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    if from.is_dir() {
        fs::create_dir_all(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
    } else {
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(from, to)?;
    }
    Ok(())
}

/// Move a file or folder; across drives, fall back to copy then delete.
fn move_path(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    copy_tree(from, to)?;
    remove_path(from)
}

impl History {
    /// Load the history stored in `file` (missing or unreadable means empty).
    /// Backups go in a `backups` folder beside it.
    pub fn load(file: &Path) -> Self {
        let backups = file.parent().unwrap_or(Path::new(".")).join("backups");
        let changes = fs::read_to_string(file)
            .map(|text| text.lines().filter_map(parse_line).collect())
            .unwrap_or_default();
        History { file: file.to_path_buf(), backups, changes }
    }

    fn save(&self) -> Result<()> {
        if let Some(parent) = self.file.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut text = String::new();
        for c in &self.changes {
            let backup = c.backup.as_ref().map(|b| b.display().to_string()).unwrap_or_else(|| "-".into());
            text.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\t{}\n",
                c.id,
                c.time,
                if c.reverted { 1 } else { 0 },
                c.action.replace(['\t', '\n', '\r'], " "),
                c.target.display(),
                backup
            ));
        }
        fs::write(&self.file, text)?;
        Ok(())
    }

    fn next_id(&self) -> u64 {
        self.changes.iter().map(|c| c.id).max().unwrap_or(0) + 1
    }

    /// Prepare to write `target`: an existing target is moved to a backup so
    /// the operation starts from a clean slate and can be undone.
    pub fn begin(&self, action: &str, target: &Path) -> Result<Pending> {
        let id = self.next_id();
        let backup = if fs::symlink_metadata(target).is_ok() {
            let name = target.file_name().map(|n| n.to_os_string()).unwrap_or_else(|| "target".into());
            let dest = self.backups.join(format!("{id}-{}", now())).join(name);
            move_path(target, &dest).map_err(|e| {
                Error::Conversion(format!("could not back up {} before writing it: {e}", target.display()))
            })?;
            Some(dest)
        } else {
            None
        };
        Ok(Pending { id, action: action.to_string(), target: target.to_path_buf(), backup })
    }

    /// Record a finished operation. On failure, remove whatever it wrote and
    /// put the backup back, so nothing changed.
    pub fn finish(&mut self, pending: Pending, ok: bool) -> Result<()> {
        if !ok {
            remove_path(&pending.target)?;
            if let Some(b) = &pending.backup {
                move_path(b, &pending.target)?;
                let _ = fs::remove_dir(b.parent().unwrap_or(b));
            }
            return Ok(());
        }
        self.changes.push(Change {
            id: pending.id,
            time: now(),
            action: pending.action,
            target: pending.target,
            backup: pending.backup,
            reverted: false,
        });
        self.save()
    }

    /// Undo a change: delete what it wrote and restore what was there before.
    pub fn revert(&mut self, id: u64) -> Result<String> {
        let Some(i) = self.changes.iter().position(|c| c.id == id) else {
            return Err(Error::Conversion(format!("change {id} is not in the history")));
        };
        let c = self.changes[i].clone();
        if c.reverted {
            return Err(Error::Conversion("that change was already reverted".into()));
        }
        if let Some(b) = &c.backup {
            if fs::symlink_metadata(b).is_err() {
                return Err(Error::Conversion(format!(
                    "the backup {} is gone, so the old version can't be restored",
                    b.display()
                )));
            }
        }
        remove_path(&c.target)?;
        let msg = match &c.backup {
            Some(b) => {
                move_path(b, &c.target)?;
                let _ = fs::remove_dir(b.parent().unwrap_or(b));
                format!("Reverted: restored the previous {}.", c.target.display())
            }
            None => format!("Reverted: deleted {}.", c.target.display()),
        };
        self.changes[i].reverted = true;
        self.changes[i].backup = None;
        self.save()?;
        Ok(msg)
    }

    /// Forget every change and delete all backups (frees their disk space).
    pub fn clear(&mut self) -> Result<()> {
        remove_path(&self.backups)?;
        self.changes.clear();
        self.save()
    }
}

fn parse_line(line: &str) -> Option<Change> {
    let f: Vec<&str> = line.split('\t').collect();
    if f.len() != 6 {
        return None;
    }
    Some(Change {
        id: f[0].parse().ok()?,
        time: f[1].parse().ok()?,
        reverted: f[2] == "1",
        action: f[3].to_string(),
        target: PathBuf::from(f[4]),
        backup: (f[5] != "-").then(|| PathBuf::from(f[5])),
    })
}

/// "just now", "5 min ago", "3 h ago", "2 days ago".
pub fn ago(time: u64) -> String {
    let secs = now().saturating_sub(time);
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86399 => format!("{} h ago", secs / 3600),
        _ => format!("{} days ago", secs / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pkgmaster-history-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn new_file_is_deleted_on_revert() {
        let d = scratch("new");
        let mut h = History::load(&d.join("state").join("history.tsv"));
        let target = d.join("out.elf");
        let p = h.begin("Converted", &target).unwrap();
        fs::write(&target, b"new").unwrap();
        h.finish(p, true).unwrap();
        assert_eq!(h.changes.len(), 1);

        let id = h.changes[0].id;
        h.revert(id).unwrap();
        assert!(!target.exists());
        assert!(History::load(&d.join("state").join("history.tsv")).changes[0].reverted);
    }

    #[test]
    fn overwritten_folder_is_restored_on_revert() {
        let d = scratch("overwrite");
        let mut h = History::load(&d.join("state").join("history.tsv"));
        let target = d.join("game-run");
        fs::create_dir_all(target.join("sub")).unwrap();
        fs::write(target.join("sub").join("old.txt"), b"old").unwrap();

        let p = h.begin("Ran AnyPS5", &target).unwrap();
        assert!(!target.exists(), "existing target is moved aside first");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("new.txt"), b"new").unwrap();
        h.finish(p, true).unwrap();

        let id = h.changes[0].id;
        h.revert(id).unwrap();
        assert_eq!(fs::read(target.join("sub").join("old.txt")).unwrap(), b"old");
        assert!(!target.join("new.txt").exists());
    }

    #[test]
    fn failed_operation_changes_nothing() {
        let d = scratch("failed");
        let mut h = History::load(&d.join("state").join("history.tsv"));
        let target = d.join("out.elf");
        fs::write(&target, b"old").unwrap();
        let p = h.begin("Converted", &target).unwrap();
        fs::write(&target, b"partial").unwrap();
        h.finish(p, false).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"old");
        assert!(h.changes.is_empty());
    }

    #[test]
    fn clear_removes_backups() {
        let d = scratch("clear");
        let mut h = History::load(&d.join("state").join("history.tsv"));
        let target = d.join("out.elf");
        fs::write(&target, b"old").unwrap();
        let p = h.begin("Converted", &target).unwrap();
        fs::write(&target, b"new").unwrap();
        h.finish(p, true).unwrap();
        h.clear().unwrap();
        assert!(h.changes.is_empty());
        assert!(!d.join("state").join("backups").exists());
        assert_eq!(fs::read(&target).unwrap(), b"new");
    }
}
