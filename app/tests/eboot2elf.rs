//! eboot2elf.py (the Python-only release) must write the same ELF as the app.
//! Skipped when no Python is on PATH.

mod common;

use std::process::Command;

use common::{build_elf, build_self, scratch, write};
use pkgmaster::elf::to_elf;

fn python() -> Option<&'static str> {
    ["python3", "python"].into_iter().find(|p| Command::new(p).arg("--version").output().is_ok_and(|o| o.status.success()))
}

#[test]
fn python_script_matches_the_app() {
    let Some(py) = python() else {
        eprintln!("no python on PATH; skipped");
        return;
    };
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/../eboot2elf.py");
    let dir = scratch("eboot2elf");
    let elf = build_elf(&[b"code segment", b"data"], false);

    let eboot = dir.join("eboot.bin");
    write(&eboot, &build_self(&elf, false));
    let run = Command::new(py).arg(script).arg(&eboot).output().unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stdout));
    let from_python = std::fs::read(dir.join("eboot.elf")).unwrap();
    assert_eq!(from_python, to_elf(&std::fs::read(&eboot).unwrap()).unwrap().data);

    // An encrypted eboot is refused, and nothing is written.
    let locked = dir.join("locked.bin");
    write(&locked, &build_self(&elf, true));
    let run = Command::new(py).arg(script).arg(&locked).output().unwrap();
    assert!(!run.status.success());
    assert!(String::from_utf8_lossy(&run.stdout).contains("decrypt this file first"));
    assert!(!dir.join("locked.elf").exists());
}
