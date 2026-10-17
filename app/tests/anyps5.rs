//! Export for AnyPS5: alignment of converted ELFs and the folder layout.

mod common;

use std::fs;

use common::{build_elf, build_self, scratch, write};
use pkgmaster::convert::{export_anyps5, load, Input, HOW_TO_RUN};
use pkgmaster::elf::{alignment_problems, parse_elf, to_elf};

#[test]
fn to_elf_keeps_offset_and_vaddr_congruent() {
    let elf = build_elf(&[&[0x90; 0x300], b"DATAdata", &[0xC3; 0x1234]], false);
    let original = parse_elf(&elf, 0).unwrap();
    assert!(alignment_problems(&original).is_empty());
    assert!(original.phdrs.iter().any(|p| p.p_offset % 0x1000 != 0), "test should cover unaligned offsets");

    let out = to_elf(&build_self(&elf, false)).unwrap();
    assert!(alignment_problems(&out.info).is_empty(), "{:?}", alignment_problems(&out.info));
    for (a, b) in original.phdrs.iter().zip(&out.info.phdrs) {
        assert_eq!((a.p_offset, a.p_vaddr, a.p_align, a.p_filesz), (b.p_offset, b.p_vaddr, b.p_align, b.p_filesz));
    }
    for p in &out.info.phdrs {
        let (o, n) = (p.p_offset as usize, p.p_filesz as usize);
        assert_eq!(&out.data[o..o + n], &elf[o..o + n]);
    }
}

#[test]
fn alignment_problems_reports_broken_input() {
    let elf = build_elf(&[&[0x90; 0x40], b"DATA"], true);
    let out = to_elf(&build_self(&elf, false)).unwrap();
    let problems = alignment_problems(&out.info);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("#1"));
}

#[test]
fn export_folder_layout() {
    let tmp = scratch("export");
    let game = tmp.join("CUSA00000");
    let eboot = build_self(&build_elf(&[&[0x90; 0x80], b"eboot data"], false), false);
    let lib_a = build_self(&build_elf(&[&[0xC3; 0x20]], false), false);
    let lib_b = build_elf(&[b"plain elf module"], false);
    let lib_c = build_self(&build_elf(&[&[0xCC; 0x20]], false), true);
    write(&game.join("eboot.bin"), &eboot);
    write(&game.join("sce_module/libA.prx"), &lib_a);
    write(&game.join("sce_module/libB.sprx"), &lib_b);
    write(&game.join("sce_module/libC.prx"), &lib_c);
    write(&game.join("sce_sys/param.json"), b"{\"titleId\": \"PPSA00000\"}");
    write(&game.join("data/level1.bin"), &[0xAB; 4096]);

    let loaded = load(&game, None).unwrap();
    let input = loaded.input.unwrap();
    assert!(matches!(input, Input::Folder(_)));
    assert_eq!(loaded.default_elf.as_deref(), Some("eboot.bin"));

    // An export folder inside the game folder would copy itself.
    assert!(export_anyps5(&input, &game.join("out"), &|_, _, _| {}).is_err());
    assert!(!game.join("out").exists());

    let out = tmp.join("export");
    let r = export_anyps5(&input, &out, &|_, _, _| {}).unwrap();
    assert_eq!(r.modules, ["libA.prx", "libB.sprx"]);
    assert_eq!(r.skipped.len(), 1);
    assert_eq!(r.skipped[0].0, "libC.prx");
    assert!(r.skipped[0].1.contains("encrypted"), "{}", r.skipped[0].1);
    assert_eq!(r.app0_files, 6);
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);

    let elf = fs::read(out.join("source/eboot.elf")).unwrap();
    assert_eq!(elf, to_elf(&eboot).unwrap().data);
    assert_eq!(fs::read(out.join("source/sce_module/libA.prx")).unwrap(), to_elf(&lib_a).unwrap().data);
    assert_eq!(fs::read(out.join("source/sce_module/libB.sprx")).unwrap(), lib_b);
    assert!(!out.join("source/sce_module/libC.prx").exists());
    for rel in ["eboot.bin", "sce_module/libA.prx", "sce_module/libC.prx", "sce_sys/param.json", "data/level1.bin"] {
        assert_eq!(fs::read(out.join("app0").join(rel)).unwrap(), fs::read(game.join(rel)).unwrap(), "{rel}");
    }
    let how = fs::read_to_string(out.join(HOW_TO_RUN)).unwrap();
    assert!(how.contains("relinker --windows source/eboot.elf app.exe"));
    assert!(how.contains("libs/*.prx"));
    assert!(how.contains("libC.prx"));

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn export_refuses_folder_without_eboot() {
    let tmp = scratch("noeboot");
    write(&tmp.join("game/sce_sys/param.json"), b"{}");
    assert!(load(&tmp.join("game"), None).is_err());
    let _ = fs::remove_dir_all(&tmp);
}
