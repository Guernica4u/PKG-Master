//! Gives the Windows .exe files the app icon.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        // Named version info: unsigned exes without it look more suspicious to antivirus heuristics.
        res.set("ProductName", "PKG Master")
            .set("FileDescription", "PKG Master: eboot.bin to ELF converter")
            .set("CompanyName", "Guernica4u")
            .set("LegalCopyright", "MIT License, Guernica4u");
        if let Err(e) = res.compile() {
            println!("cargo:warning=could not embed the app icon: {e}");
        }
    }
}
