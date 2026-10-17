//! Minimal command-line front end, used to check the Rust core against the
//! Python implementation and for scripting.
//!
//!   pkgmaster-cli info    FILE [--passcode P]
//!   pkgmaster-cli list    FILE [--passcode P]
//!   pkgmaster-cli convert FILE OUT [--passcode P] [--path P]
//!   pkgmaster-cli extract FILE DIR [--passcode P]
//!
//! Exit codes match the Python CLI: 0 ok, 2 bad file/usage, 3 key required or
//! wrong, 4 recognised but unimplemented.

use std::path::Path;
use std::process::ExitCode;

use pkgmaster::convert::{convert_to_elf, extract_all, load, Input};
use pkgmaster::Error;

fn usage() -> ExitCode {
    eprintln!("usage: pkgmaster-cli info|list|convert|extract FILE [OUT] [--passcode P] [--path P]");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut take_opt = |name: &str| -> Option<String> {
        let i = args.iter().position(|a| a == name)?;
        let value = args.get(i + 1).cloned();
        args.drain(i..(i + 2).min(args.len()));
        value
    };
    let passcode = take_opt("--passcode").or_else(|| std::env::var("PKGMASTER_PASSCODE").ok());
    let inner = take_opt("--path");
    if args.len() < 2 {
        return usage();
    }
    let quiet = |_: u64, _: u64, _: &str| {};
    let result = (|| -> Result<(), Error> {
        let loaded = load(Path::new(&args[1]), passcode.as_deref())?;
        if let Some(e) = loaded.key_problem {
            return Err(e);
        }
        let input = loaded.input.expect("loaded without a key problem");
        match args[0].as_str() {
            "info" => {
                for (k, v) in &loaded.rows {
                    println!("{k:<14}  {v}");
                }
                if let Some(d) = &loaded.default_elf {
                    println!("{:<14}  {d}", "Executable");
                }
            }
            "list" => {
                for f in loaded.files.iter().filter(|f| !f.is_dir) {
                    println!("{:>12}  {}", f.size, f.path);
                }
            }
            "convert" => {
                let out = args.get(2).ok_or_else(|| Error::Conversion("convert needs an output path".into()))?;
                let r = convert_to_elf(&input, inner.as_deref(), Path::new(out), &quiet)?;
                println!("source    {}", r.source);
                println!("output    {} ({} bytes)", r.output.display(), r.conversion.data.len());
                println!(
                    "elf       {} / {} / {}",
                    r.conversion.info.type_name(),
                    r.conversion.info.machine_name(),
                    r.conversion.info.osabi_name()
                );
                println!("entry     0x{:X}", r.conversion.info.e_entry);
                for w in &r.conversion.warnings {
                    println!("warning: {w}");
                }
            }
            "extract" => {
                let out = args.get(2).ok_or_else(|| Error::Conversion("extract needs a folder".into()))?;
                let Input::Package(pfs) = &input else {
                    return Err(Error::Conversion("only packages can be extracted".into()));
                };
                let n = extract_all(pfs, Path::new(out), &quiet)?;
                println!("{n} file(s) written to {out}");
            }
            _ => return Err(Error::Conversion(format!("unknown command {:?}", args[0]))),
        }
        Ok(())
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(e.exit_code() as u8)
        }
    }
}
