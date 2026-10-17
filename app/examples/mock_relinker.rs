//! Stand-in for AnyPS5's relinker, used only by tests/anyps5_run.rs.
//!
//! As the relinker it records its arguments in `relinker-args.txt`, prints a
//! line on stdout and on stderr, copies the converted modules from `source/`
//! into `app0/` and copies itself to the output path. Run as that output
//! (no arguments) it prints where it runs and exits. It fails with exit code 3
//! when `app0/mock-fail` exists.

use std::fs;
use std::path::Path;
use std::process::ExitCode;

fn copy_modules(from: &Path, to: &Path) {
    let Ok(entries) = fs::read_dir(from) else { return };
    for e in entries.flatten() {
        let dest = to.join(e.file_name());
        if e.path().is_dir() {
            copy_modules(&e.path(), &dest);
        } else {
            fs::create_dir_all(to).unwrap();
            fs::copy(e.path(), dest).unwrap();
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cwd = std::env::current_dir().unwrap();
    if args.is_empty() {
        println!("mock app running in {}", cwd.display());
        eprintln!("mock app stderr");
        return ExitCode::SUCCESS;
    }
    fs::write("relinker-args.txt", args.join("\n")).unwrap();
    println!("mock relinker: {}", args.join(" "));
    // Progress redrawn in place, as the real relinker does.
    println!("Instruction scan | eboot.elf |   0%\rInstruction scan | eboot.elf |  57%\rInstruction scan | eboot.elf | 100%");
    eprintln!("mock relinker stderr line");
    if Path::new("app0/mock-fail").exists() {
        eprintln!("mock relinker: failing on purpose");
        return ExitCode::from(3);
    }
    let input = Path::new(&args[args.len() - 2]);
    let source = input.parent().unwrap();
    let mut dirs = vec!["sce_module".to_string()];
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--module-dir" {
            dirs.push(it.next().unwrap().clone());
        }
    }
    for d in dirs {
        copy_modules(&source.join(&d), &Path::new("app0").join(&d));
    }
    fs::copy(std::env::current_exe().unwrap(), &args[args.len() - 1]).unwrap();
    ExitCode::SUCCESS
}
