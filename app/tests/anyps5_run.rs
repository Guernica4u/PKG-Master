//! Run in AnyPS5: PS4 detection, the relinker's source folder, settings, and
//! a full run against examples/mock_relinker.rs standing in for AnyPS5.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use common::{build_elf, build_self, scratch, write};
use pkgmaster::anyps5::{
    find_libs, find_release, find_relinker, launch_command, preflight, prepare_source, relinker_args, run_logged, run_relinker, sfo_string,
    Settings,
};
use pkgmaster::elf::to_elf;

/// A PARAM.SFO holding UTF-8 string values.
fn build_sfo(pairs: &[(&str, &str)]) -> Vec<u8> {
    let (mut keys, mut data, mut index) = (Vec::new(), Vec::new(), Vec::new());
    for (k, v) in pairs {
        let value = [v.as_bytes(), b"\0"].concat();
        index.extend_from_slice(&(keys.len() as u16).to_le_bytes());
        index.extend_from_slice(&0x0204u16.to_le_bytes());
        index.extend_from_slice(&(value.len() as u32).to_le_bytes());
        index.extend_from_slice(&(value.len() as u32).to_le_bytes());
        index.extend_from_slice(&(data.len() as u32).to_le_bytes());
        keys.extend_from_slice(k.as_bytes());
        keys.push(0);
        data.extend_from_slice(&value);
    }
    let key_start = 0x14 + index.len();
    let data_start = key_start + keys.len();
    let mut out = b"\0PSF".to_vec();
    out.extend_from_slice(&0x0101u32.to_le_bytes());
    out.extend_from_slice(&(key_start as u32).to_le_bytes());
    out.extend_from_slice(&(data_start as u32).to_le_bytes());
    out.extend_from_slice(&(pairs.len() as u32).to_le_bytes());
    out.extend(index);
    out.extend(keys);
    out.extend(data);
    out
}

fn ps5_module(byte: u8) -> Vec<u8> {
    // A PS5 SELF has a different magic; a plain ELF is enough for a PS5 dump.
    build_elf(&[&[byte; 0x20]], false)
}

/// Every file under `root` with its contents, to prove nothing was changed.
fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in fs::read_dir(dir).unwrap().flatten() {
            if e.path().is_dir() {
                stack.push(e.path());
            } else {
                out.push((e.path(), fs::read(e.path()).unwrap()));
            }
        }
    }
    out.sort();
    out
}

/// A PS4 Unity game like the user's dump: eboot.elf only (no eboot.bin),
/// PS4 SELF modules, Unity modules under Media/, param.sfo but no param.json.
fn ps4_unity_game(root: &Path) -> Vec<u8> {
    let eboot = build_elf(&[&[0x90; 0x80], b"eboot data"], false);
    write(&root.join("eboot.elf"), &eboot);
    write(&root.join("sce_module/libc.prx"), &build_self(&build_elf(&[&[0xC3; 0x20]], false), false));
    write(&root.join("sce_module/libEnc.prx"), &build_self(&build_elf(&[&[0xCC; 0x20]], false), true));
    write(&root.join("Media/Modules/Il2CppUserAssemblies.prx"), &build_self(&build_elf(&[b"il2cpp"], false), false));
    write(&root.join("Media/Modules/notes.txt"), b"not a module");
    write(&root.join("Media/Plugins/sub/SavedGames.sprx"), &build_self(&build_elf(&[b"saves"], false), false));
    write(&root.join("Media/level0"), &[0xAB; 4096]);
    write(&root.join("sce_sys/param.sfo"), &build_sfo(&[("CATEGORY", "gd"), ("TITLE_ID", "CUSA10969")]));
    write(&root.join("sce_sys/about/right.sprx"), &build_self(&build_elf(&[b"right"], false), false));
    eboot
}

#[test]
fn sfo_strings() {
    let sfo = build_sfo(&[("APP_VER", "01.00"), ("TITLE_ID", "CUSA10969")]);
    assert_eq!(sfo_string(&sfo, "TITLE_ID").as_deref(), Some("CUSA10969"));
    assert_eq!(sfo_string(&sfo, "APP_VER").as_deref(), Some("01.00"));
    assert_eq!(sfo_string(&sfo, "CONTENT_ID"), None);
    assert_eq!(sfo_string(b"not an sfo", "TITLE_ID"), None);
    assert_eq!(sfo_string(&sfo[..0x20], "TITLE_ID"), None);
}

#[test]
fn detects_ps4_titles() {
    let tmp = scratch("ps4detect");

    // By title ID.
    let game = tmp.join("sfo");
    ps4_unity_game(&game);
    let pre = preflight(&game).unwrap();
    assert_eq!(pre.title_id.as_deref(), Some("CUSA10969"));
    assert!(pre.ps4.as_deref().unwrap().contains("CUSA10969"));
    assert!(!pre.has_param_json);
    assert_eq!(pre.module_dirs, ["Media/Modules", "Media/Plugins"]);
    let w = pre.warnings();
    assert_eq!(w.len(), 2, "{w:?}");
    assert!(w[0].contains("PS5 (PPSA)") && w[0].contains("expected to fail"));
    assert!(w[1].contains("param.json"));

    // By the PS4 SELF magic, with no param.sfo.
    let game = tmp.join("magic");
    write(&game.join("eboot.elf"), &build_elf(&[b"x"], false));
    write(&game.join("sce_module/libA.prx"), &build_self(&build_elf(&[b"a"], false), false));
    let pre = preflight(&game).unwrap();
    assert_eq!(pre.title_id, None);
    assert!(pre.ps4.as_deref().unwrap().contains("sce_module/libA.prx"), "{:?}", pre.ps4);
    assert!(pre.module_dirs.is_empty());

    // A PS5 title with param.json gets no warnings; lower-case media/ is found.
    let game = tmp.join("ps5");
    write(&game.join("eboot.bin"), &ps5_module(1));
    write(&game.join("sce_module/libB.prx"), &ps5_module(2));
    write(&game.join("media/plugins/p.prx"), &ps5_module(3));
    write(&game.join("sce_sys/param.json"), br#"{"titleId":"PPSA01234"}"#);
    write(&game.join("sce_sys/param.sfo"), &build_sfo(&[("TITLE_ID", "PPSA01234")]));
    let pre = preflight(&game).unwrap();
    assert_eq!(pre.ps4, None);
    assert!(pre.warnings().is_empty(), "{:?}", pre.warnings());
    assert_eq!(pre.module_dirs, ["media/plugins"]);
    assert_eq!(
        relinker_args(&pre),
        ["--windows", "--module-dir", "media/plugins", "source/eboot.elf", "app.exe"]
    );

    assert!(preflight(&tmp.join("missing")).is_err());
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn source_folder_preparation() {
    let tmp = scratch("source");
    let game = tmp.join("game");
    let eboot = ps4_unity_game(&game);
    let before = snapshot(&game);

    let pre = preflight(&game).unwrap();
    let source = tmp.join("source");
    let r = prepare_source(&game, &source, &pre.module_dirs, &|_, _, _| {}).unwrap();
    assert_eq!(r.eboot_from, "eboot.elf");
    assert_eq!(fs::read(source.join("eboot.elf")).unwrap(), eboot);
    assert_eq!(
        r.modules,
        ["sce_module/libc.prx", "Media/Modules/Il2CppUserAssemblies.prx", "Media/Plugins/sub/SavedGames.sprx"]
    );
    assert_eq!(r.skipped.len(), 1);
    assert_eq!(r.skipped[0].0, "sce_module/libEnc.prx");
    for m in &r.modules {
        let converted = fs::read(source.join(m)).unwrap();
        assert_eq!(converted, to_elf(&fs::read(game.join(m)).unwrap()).unwrap().data, "{m}");
        assert_eq!(&converted[..4], b"\x7fELF");
    }
    assert!(!source.join("sce_module/libEnc.prx").exists());
    assert!(!source.join("Media/Modules/notes.txt").exists());
    assert!(!source.join("Media/level0").exists());
    assert_eq!(snapshot(&game), before, "the game folder must not change");

    // eboot.bin wins over eboot.elf when both exist.
    let other = build_self(&build_elf(&[b"from eboot.bin"], false), false);
    write(&game.join("eboot.bin"), &other);
    let r = prepare_source(&game, &tmp.join("source2"), &[], &|_, _, _| {}).unwrap();
    assert_eq!(r.eboot_from, "eboot.bin");
    assert_eq!(fs::read(tmp.join("source2/eboot.elf")).unwrap(), to_elf(&other).unwrap().data);
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn settings_round_trip() {
    let tmp = scratch("settings");
    let path = tmp.join("deep/settings.ini");
    assert_eq!(Settings::load(&path), Settings::default());
    let s = Settings {
        anyps5_dir: Some(PathBuf::from(r"C:\Tools\prx-windows-v0.1.2")),
        relinker: Some(PathBuf::from(r"C:\Users\me\Downloads\relinker-v0.1.2.exe")),
        skip_version: Some("v9.9.9".into()),
        anyps5_version: Some("v0.1.3".into()),
        rpcs3: Some(PathBuf::from(r"D:\Emulators\RPCS3\rpcs3.exe")),
    };
    s.save(&path).unwrap();
    assert_eq!(Settings::load(&path), s);
    let _ = fs::remove_dir_all(&tmp);
}

fn mock_relinker() -> PathBuf {
    // target/debug/deps/<test>.exe -> target/debug/examples/mock_relinker.exe
    let deps = std::env::current_exe().unwrap().parent().unwrap().to_path_buf();
    let exe = deps.parent().unwrap().join("examples").join(format!("mock_relinker{}", std::env::consts::EXE_SUFFIX));
    assert!(exe.is_file(), "{} was not built; run `cargo test`", exe.display());
    exe
}

const RELINKER_NAME: &str = if cfg!(windows) { "relinker-v0.1.2.exe" } else { "relinker-v0.1.2" };

/// Copy the mock relinker to `path`.
fn put_relinker(path: &Path) {
    write(path, &fs::read(mock_relinker()).unwrap());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// AnyPS5's libraries archive as released (prx-windows-v0.1.2): libs/ with
/// .prx files and the MinGW runtime DLLs, and no relinker.
fn mock_libs(dir: &Path) {
    write(&dir.join("USAGE.md"), b"# Relinker usage");
    write(&dir.join("libs/libkernel.prx"), b"anyps5 kernel");
    write(&dir.join("libs/libSceLibcInternal.prx"), b"anyps5 libc");
    write(&dir.join("libs/libgcc_s_seh-1.dll"), b"mingw runtime");
    write(&dir.join("libs/README.txt"), b"not a library");
}

/// The libraries archive with the versioned relinker dropped into it.
fn mock_release(dir: &Path) {
    mock_libs(dir);
    put_relinker(&dir.join(RELINKER_NAME));
}

#[test]
fn find_release_checks_the_folder() {
    let tmp = scratch("release");
    assert!(find_release(&tmp, None).is_err());

    // The versioned relinker inside the libraries folder.
    mock_release(&tmp.join("ok"));
    let r = find_release(&tmp.join("ok"), None).unwrap();
    assert_eq!(r.libs, tmp.join("ok/libs"));
    assert_eq!(r.relinker, tmp.join("ok").join(RELINKER_NAME));

    // The libraries archive alone has no relinker; it can be given separately,
    // as when relinker-v0.1.2.exe sits in Downloads next to the unpacked folder.
    mock_libs(&tmp.join("prx-windows"));
    assert!(find_libs(&tmp.join("prx-windows")).is_ok());
    assert_eq!(find_relinker(&tmp.join("prx-windows")), None);
    let err = find_release(&tmp.join("prx-windows"), None).unwrap_err().to_string();
    assert!(err.contains("relinker"), "{err}");
    put_relinker(&tmp.join(RELINKER_NAME));
    let r = find_release(&tmp.join("prx-windows"), Some(&tmp.join(RELINKER_NAME))).unwrap();
    assert_eq!(r.relinker, tmp.join(RELINKER_NAME));
    // A remembered relinker that has since been deleted is not used.
    assert!(find_release(&tmp.join("prx-windows"), Some(&tmp.join("gone.exe"))).is_err());
    let s = Settings { anyps5_dir: Some(tmp.join("prx-windows")), relinker: Some(tmp.join(RELINKER_NAME)), ..Default::default() };
    assert_eq!(s.release().unwrap().relinker, tmp.join(RELINKER_NAME));

    // A relinker with no libs/ is not enough.
    put_relinker(&tmp.join("nolibs").join(RELINKER_NAME));
    assert!(find_release(&tmp.join("nolibs"), None).unwrap_err().to_string().contains("libs"));
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn run_with_mock_relinker() {
    let tmp = scratch("run");
    let game = tmp.join("game");
    ps4_unity_game(&game);
    write(&game.join("eboot.bin"), &build_self(&build_elf(&[b"original eboot"], false), false));
    let before = snapshot(&game);
    mock_release(&tmp.join("AnyPS5"));
    let release = find_release(&tmp.join("AnyPS5"), None).unwrap();

    let lines = Mutex::new(Vec::<String>::new());
    let log = |l: &str| lines.lock().unwrap().push(l.to_string());

    // A run folder inside the game folder would copy itself.
    assert!(run_relinker(&game, &release, &game.join("run"), &|_, _, _| {}, &log).is_err());
    assert!(!game.join("run").exists());

    let out = tmp.join("run");
    let r = run_relinker(&game, &release, &out, &|_, _, _| {}, &log).unwrap();
    let expected = [
        "--windows",
        "--module-dir",
        "Media/Modules",
        "--module-dir",
        "Media/Plugins",
        "source/eboot.elf",
        "app.exe",
    ];
    assert_eq!(r.args, expected);
    assert_eq!(fs::read_to_string(out.join("relinker-args.txt")).unwrap(), expected.join("\n"));
    let log_text = lines.lock().unwrap().join("\n");
    assert!(log_text.contains("mock relinker: --windows"), "{log_text}");
    assert!(log_text.contains("mock relinker stderr line"), "{log_text}");
    assert!(lines.lock().unwrap().iter().any(|l| l == "Instruction scan | eboot.elf | 100%"), "{log_text}");
    assert!(!log_text.contains("57%"), "{log_text}");

    // Runtime layout from AnyPS5's USAGE.md.
    assert!(out.join("app.exe").is_file());
    assert_eq!(r.libs, 3);
    assert_eq!(fs::read(out.join("libs/libgcc_s_seh-1.dll")).unwrap(), b"mingw runtime");
    assert_eq!(fs::read(out.join("libs/libkernel.prx")).unwrap(), b"anyps5 kernel");
    assert!(out.join("libs/libSceLibcInternal.prx").is_file());
    assert!(!out.join("libs/README.txt").exists());
    for rel in ["eboot.bin", "eboot.elf", "sce_sys/param.sfo", "sce_sys/about/right.sprx", "Media/level0"] {
        assert_eq!(fs::read(out.join("app0").join(rel)).unwrap(), fs::read(game.join(rel)).unwrap(), "{rel}");
    }
    assert!(!out.join("app0/sce_sys/param.json").exists(), "no param.json may be made up");
    // The (mock) relinker put the converted modules under app0/.
    assert_eq!(
        fs::read(out.join("app0/Media/Plugins/sub/SavedGames.sprx")).unwrap(),
        fs::read(out.join("source/Media/Plugins/sub/SavedGames.sprx")).unwrap()
    );
    assert_eq!(snapshot(&game), before, "the game folder must not change");

    // Launch runs app.exe from the run folder and its output can be streamed.
    lines.lock().unwrap().clear();
    let status = run_logged(launch_command(&out).unwrap(), &log).unwrap();
    assert!(status.success());
    let log_text = lines.lock().unwrap().join("\n");
    let canonical = fs::canonicalize(&out).unwrap();
    assert!(
        log_text.contains(&format!("mock app running in {}", out.display()))
            || log_text.contains(&format!("mock app running in {}", canonical.display())),
        "{log_text}"
    );
    assert!(log_text.contains("mock app stderr"));
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn relinker_failure_is_reported() {
    let tmp = scratch("runfail");
    let game = tmp.join("game");
    ps4_unity_game(&game);
    write(&game.join("mock-fail"), b"");
    mock_release(&tmp.join("AnyPS5"));
    let release = find_release(&tmp.join("AnyPS5"), None).unwrap();
    let lines = Mutex::new(Vec::<String>::new());
    let err = run_relinker(&game, &release, &tmp.join("run"), &|_, _, _| {}, &|l: &str| {
        lines.lock().unwrap().push(l.to_string())
    })
    .err()
    .expect("the relinker failed");
    assert!(err.to_string().contains("relinker failed"), "{err}");
    assert!(lines.lock().unwrap().iter().any(|l| l.contains("failing on purpose")));
    assert!(!tmp.join("run/app.exe").exists());
    assert!(launch_command(&tmp.join("run")).is_err());
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn folder_with_only_eboot_elf_opens() {
    let tmp = scratch("ebootelf");
    let game = tmp.join("game");
    ps4_unity_game(&game);
    let loaded = pkgmaster::convert::load(&game, None).unwrap();
    assert_eq!(loaded.default_elf.as_deref(), Some("eboot.elf"));
    let _ = fs::remove_dir_all(&tmp);
}
