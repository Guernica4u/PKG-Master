//! Dumped PS3 game folders: both layouts are recognised, and their title,
//! title ID and icon come from PARAM.SFO and ICON0.PNG.

use std::fs;
use std::path::{Path, PathBuf};

use pkgmaster::convert::{load, ps3_game_dir};

/// A minimal PARAM.SFO with TITLE and TITLE_ID.
fn param_sfo(title: &str, id: &str) -> Vec<u8> {
    let entries = [("TITLE", format!("{title}\0")), ("TITLE_ID", format!("{id}\0"))];
    let (mut keys, mut data, mut index) = (Vec::new(), Vec::new(), Vec::new());
    for (k, v) in &entries {
        index.push((keys.len() as u16, v.len() as u32, data.len() as u32));
        keys.extend_from_slice(k.as_bytes());
        keys.push(0);
        data.extend_from_slice(v.as_bytes());
    }
    let key_table = 20 + index.len() * 16;
    let data_table = key_table + keys.len();
    let mut out = b"\0PSF".to_vec();
    out.extend_from_slice(&0x0101u32.to_le_bytes());
    out.extend_from_slice(&(key_table as u32).to_le_bytes());
    out.extend_from_slice(&(data_table as u32).to_le_bytes());
    out.extend_from_slice(&(index.len() as u32).to_le_bytes());
    for (ko, len, doff) in index {
        out.extend_from_slice(&ko.to_le_bytes());
        out.extend_from_slice(&0x0204u16.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&doff.to_le_bytes());
    }
    out.extend_from_slice(&keys);
    out.extend_from_slice(&data);
    out
}

fn make_game(dir: &Path, title: &str, id: &str) {
    fs::create_dir_all(dir.join("USRDIR")).unwrap();
    fs::write(dir.join("PARAM.SFO"), param_sfo(title, id)).unwrap();
    fs::write(dir.join("USRDIR").join("EBOOT.BIN"), b"SCE\0not a real eboot").unwrap();
    let mut png = Vec::new();
    image::RgbaImage::from_pixel(8, 8, image::Rgba([9, 9, 9, 255]))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    fs::write(dir.join("ICON0.PNG"), png).unwrap();
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("pkgmaster-ps3-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn disc_dump_layout() {
    let root = scratch("disc");
    make_game(&root.join("PS3_GAME"), "Demon's Souls", "BLUS30443");
    assert_eq!(ps3_game_dir(&root), Some(root.join("PS3_GAME")));
    let l = load(&root, None).unwrap();
    assert_eq!(l.info.title.as_deref(), Some("Demon's Souls"));
    assert_eq!(l.info.title_id.as_deref(), Some("BLUS30443"));
    assert!(l.info.icon.is_some());
    assert_eq!(l.ps3_eboot, Some(root.join("PS3_GAME").join("USRDIR").join("EBOOT.BIN")));
    assert_eq!(l.default_elf, None);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn psn_game_layout() {
    let root = scratch("psn");
    make_game(&root, "Flower", "NPUA80083");
    assert_eq!(ps3_game_dir(&root), Some(root.clone()));
    let l = load(&root, None).unwrap();
    assert_eq!(l.info.title_id.as_deref(), Some("NPUA80083"));
    assert!(l.ps3_eboot.is_some());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn ps4_folder_is_not_ps3() {
    let root = scratch("ps4");
    fs::write(root.join("eboot.bin"), b"x").unwrap();
    assert_eq!(ps3_game_dir(&root), None);
    let _ = fs::remove_dir_all(&root);
}
