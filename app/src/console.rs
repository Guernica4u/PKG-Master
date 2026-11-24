//! Which console a game file or folder is for, from its extension and the
//! disc headers: PS1, PS2, PSP, PS Vita, GameCube, Wii and Xbox 360. (PS3,
//! PS4 and PS5 games are handled by the package and folder code.)

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Console {
    Ps1,
    Ps2,
    Psp,
    Vita,
    GameCube,
    Wii,
    Xbox360,
}

impl Console {
    pub fn name(self) -> &'static str {
        match self {
            Console::Ps1 => "PS1",
            Console::Ps2 => "PS2",
            Console::Psp => "PSP",
            Console::Vita => "PS Vita",
            Console::GameCube => "GameCube",
            Console::Wii => "Wii",
            Console::Xbox360 => "Xbox 360",
        }
    }
}

/// File extensions the Library scan picks up as games (lowercase).
pub const ROM_EXTS: [&str; 13] = ["iso", "cue", "chd", "cso", "pbp", "vpk", "rvz", "gcz", "wbfs", "wia", "ciso", "gcm", "zar"];

/// Disc images bigger than a CD are DVDs, so a CHD this size is a PS2 game.
const CD_MAX: u64 = 900 << 20;

/// The console `path` is for, or `None` if it isn't a game this knows.
pub fn detect(path: &Path) -> Option<Console> {
    if path.is_dir() {
        return path.join("default.xex").is_file().then_some(Console::Xbox360);
    }
    let name = path.file_name()?.to_string_lossy().to_lowercase();
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_string()).unwrap_or_default();
    match ext.as_str() {
        "cue" => Some(Console::Ps1),
        "cso" | "pbp" => Some(Console::Psp),
        "vpk" => Some(Console::Vita),
        "xex" | "zar" => Some(Console::Xbox360),
        // shortcut: a CHD doesn't say which console it's for; size guesses CD (PS1) vs DVD (PS2).
        "chd" => Some(if path.metadata().ok()?.len() > CD_MAX { Console::Ps2 } else { Console::Ps1 }),
        "rvz" | "gcz" | "wbfs" | "wia" | "ciso" | "gcm" | "iso" | "bin" if name != "eboot.bin" => sniff(path, &ext),
        _ => None,
    }
}

/// Read the disc headers of an image to tell the consoles apart.
fn sniff(path: &Path, ext: &str) -> Option<Console> {
    let mut f = File::open(path).ok()?;
    let mut at = |offset: u64, len: usize| -> Option<Vec<u8>> {
        f.seek(SeekFrom::Start(offset)).ok()?;
        let mut buf = vec![0u8; len];
        f.read_exact(&mut buf).ok()?;
        Some(buf)
    };
    // Dolphin's own formats keep the GameCube/Wii disc header inside.
    if matches!(ext, "rvz" | "gcz" | "wia" | "ciso" | "wbfs") {
        return Some(if ext == "wbfs" { Console::Wii } else { at(0x58, 0x20).and_then(|h| gc_or_wii(&h)).unwrap_or(Console::Wii) });
    }
    if let Some(c) = at(0, 0x20).and_then(|h| gc_or_wii(&h)) {
        return Some(c);
    }
    // Xbox 360 discs: the game partition sits at one of these offsets.
    for base in [0u64, 0x2080000, 0xFD90000, 0x18300000] {
        if at(base + 0x10000, 20).is_some_and(|m| &m == b"MICROSOFT*XBOX*MEDIA") {
            return Some(Console::Xbox360);
        }
    }
    // ISO 9660: the system ID names PSP discs; PS1/PS2 discs boot from SYSTEM.CNF.
    if at(0x8008, 8).is_some_and(|s| &s == b"PSP GAME") {
        return Some(Console::Psp);
    }
    // SYSTEM.CNF sits near the start of the disc (raw .bin images included).
    let head = at(0, 8 << 20).or_else(|| {
        let len = path.metadata().ok()?.len().min(8 << 20) as usize;
        at(0, len)
    })?;
    let has = |needle: &[u8]| head.windows(needle.len()).any(|w| w == needle);
    if has(b"BOOT2") {
        Some(Console::Ps2)
    } else if has(b"cdrom:") && has(b"BOOT") {
        Some(Console::Ps1)
    } else {
        None
    }
}

/// GameCube and Wii disc headers carry a magic number at 0x1C and 0x18.
fn gc_or_wii(h: &[u8]) -> Option<Console> {
    if h.len() < 0x20 {
        return None;
    }
    if h[0x18..0x1C] == [0x5D, 0x1C, 0x9E, 0xA3] {
        Some(Console::Wii)
    } else if h[0x1C..0x20] == [0xC2, 0x33, 0x9F, 0x3D] {
        Some(Console::GameCube)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn tells_consoles_apart() {
        let dir = std::env::temp_dir().join(format!("pkgmaster-console-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("x360")).unwrap();
        fs::write(dir.join("x360/default.xex"), b"XEX2").unwrap();
        let file = |name: &str, bytes: &[(usize, &[u8])], len: usize| {
            let mut d = vec![0u8; len];
            for (at, b) in bytes {
                d[*at..*at + b.len()].copy_from_slice(b);
            }
            let p = dir.join(name);
            fs::write(&p, d).unwrap();
            p
        };
        let gc = file("gc.iso", &[(0x1C, &[0xC2, 0x33, 0x9F, 0x3D])], 0x100);
        let wii = file("wii.iso", &[(0x18, &[0x5D, 0x1C, 0x9E, 0xA3])], 0x100);
        let psp = file("psp.iso", &[(0x8008, b"PSP GAME")], 0x9000);
        let ps2 = file("ps2.iso", &[(0x9000, b"BOOT2 = cdrom0:\\SLUS_200.62;1")], 0xA000);
        let ps1 = file("ps1.bin", &[(0x9000, b"BOOT = cdrom:\\SCUS_944.26;1")], 0xA000);
        let x360 = file("x.iso", &[(0x10000, b"MICROSOFT*XBOX*MEDIA")], 0x10100);
        assert_eq!(detect(&gc), Some(Console::GameCube));
        assert_eq!(detect(&wii), Some(Console::Wii));
        assert_eq!(detect(&psp), Some(Console::Psp));
        assert_eq!(detect(&ps2), Some(Console::Ps2));
        assert_eq!(detect(&ps1), Some(Console::Ps1));
        assert_eq!(detect(&x360), Some(Console::Xbox360));
        assert_eq!(detect(&dir.join("x360")), Some(Console::Xbox360));
        assert_eq!(detect(&file("game.cue", &[], 4)), Some(Console::Ps1));
        assert_eq!(detect(&file("game.vpk", &[], 4)), Some(Console::Vita));
        assert_eq!(detect(&file("eboot.bin", &[], 4)), None);
        assert_eq!(detect(&file("blank.iso", &[], 0x100)), None);
        let _ = fs::remove_dir_all(&dir);
    }
}
