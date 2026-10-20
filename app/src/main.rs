//! PKG Master: a small window for opening a PS4/PS5 package, converting its
//! executable to ELF, extracting all of its files, or exporting a package or
//! decrypted game folder for AnyPS5, and running a game folder through the
//! user's own AnyPS5 relinker.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use eframe::egui::{self, Color32, RichText};
use pkgmaster::anyps5::{
    find_libs, find_relinker, install_release, is_ps4_self, launch_command, preflight, run_relinker, settings_path, spawn_piped,
    stream_lines, Settings, ANYPS5_RELEASES, ANYPS5_REPO, APP_EXE,
};
use pkgmaster::binary::fmt_size;
use pkgmaster::convert::{
    convert_to_elf, default_output_name, extract_all, load, ps3_game_dir, Input, Loaded,
};
use pkgmaster::download::{Release, CANCELLED};
use pkgmaster::history::{ago, History};
use pkgmaster::library::{Entry, Library};
use pkgmaster::update;
use pkgmaster::Error;

const PACKAGE_EXTS: [&str; 1] = ["pkg"];
const BINARY_EXTS: [&str; 5] = ["bin", "self", "sprx", "prx", "elf"];

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("PKG Master")
            .with_inner_size([760.0, 560.0])
            .with_min_inner_size([520.0, 400.0])
            .with_drag_and_drop(true)
            .with_icon(window_icon()),
        ..Default::default()
    };
    update::clean_up_old_files();
    // A file passed on the command line (or dropped onto the .exe) opens at start.
    let initial = std::env::args_os().nth(1).map(PathBuf::from);
    eframe::run_native(
        "PKG Master",
        options,
        Box::new(move |cc| {
            let mut app = App::new();
            app.check_for_update(&cc.egui_ctx);
            if let Some(path) = initial {
                app.start_load(&cc.egui_ctx, path);
            }
            Ok(Box::new(app))
        }),
    )
}

/// The app icon for the title bar and taskbar.
fn window_icon() -> egui::IconData {
    let img = image::load_from_memory_with_format(include_bytes!("../assets/icon.png"), image::ImageFormat::Png)
        .expect("the bundled icon is a PNG")
        .to_rgba8();
    egui::IconData { width: img.width(), height: img.height(), rgba: img.into_raw() }
}

enum Msg {
    /// A load finished; the flag says whether it is a PS4 game.
    Loaded(PathBuf, Result<Loaded, Error>, bool),
    /// "Download for me" or an AnyPS5 update finished: the AnyPS5 folder
    /// and release tag, or why not.
    AnyPs5Installed(Result<(PathBuf, String), Error>),
    /// A newer AnyPS5 release than the installed one (its tag).
    AnyPs5UpdateFound(String),
    /// A newer PKG Master is on GitHub.
    UpdateFound(Release),
    /// "Update now" finished: the new program to start, or why not.
    Updated(Result<PathBuf, Error>),
    Progress(u64, u64, String),
    Done(Result<String, Error>),
    Log(String),
    /// The relinker finished; the runtime folder is ready to launch.
    Relinked(Result<PathBuf, Error>),
    /// The launched game exited.
    Exited(String),
}

/// Most log lines kept in the window.
const LOG_LIMIT: usize = 5000;

enum Status {
    Info(String),
    Ok(String),
    Err(String),
}

struct App {
    path: Option<PathBuf>,
    passcode: String,
    loaded: Option<Loaded>,
    selected: Option<String>,
    busy: bool,
    progress: Option<(u64, u64, String)>,
    status: Status,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    settings: Settings,
    /// Warnings shown before a run: (warnings, game folder, run folder).
    /// The run starts once the user continues.
    confirm: Option<(Vec<String>, PathBuf, PathBuf)>,
    /// The last laid-out AnyPS5 runtime folder.
    run_dir: Option<PathBuf>,
    log: Vec<String>,
    /// The launched game, while it runs.
    game: Arc<Mutex<Option<Child>>>,
    /// The listing of an opened folder that is not itself a game.
    browse: Option<Vec<BrowseRow>>,
    /// Everything the app has written, for the History panel.
    history: Arc<Mutex<History>>,
    show_history: bool,
    /// A revert waiting for confirmation: (id, target, had a backup).
    revert_confirm: Option<(u64, PathBuf, bool)>,
    clear_confirm: bool,
    /// A file or folder waiting for confirmation before going to the Recycle Bin.
    delete_confirm: Option<PathBuf>,
    /// Reload what is open once the running job finishes (after a delete).
    refresh_after_job: bool,
    pending_refresh: bool,
    /// The open game's icon, uploaded to the GPU once per load.
    icon_tex: Option<egui::TextureHandle>,
    /// The game title currently shown in the window's title bar.
    shown_title: Option<String>,
    /// Run shadPKG was pressed but shadPKG.exe isn't installed.
    shadpkg_missing: bool,
    /// An AnyPS5 button was pressed but AnyPS5 isn't set up.
    anyps5_missing: bool,
    /// The open game is a PS4 title, so the AnyPS5 buttons are hidden.
    is_ps4: bool,
    /// Set to stop the running download.
    cancel: Arc<AtomicBool>,
    /// The running job is a download that Cancel can stop.
    cancellable: bool,
    /// A newer release, once the startup check finds one.
    update: Option<Release>,
    show_update: bool,
    /// When Copy log was last pressed (egui time), to show "Copied".
    copied_at: Option<f64>,
    /// Starred games and games run in AnyPS5 or RPCS3.
    library: Library,
    /// Library icons uploaded to the GPU, by icon file (None: unreadable).
    library_tex: HashMap<PathBuf, Option<egui::TextureHandle>>,
    /// What was open before, for the Back button (newest last).
    back_stack: Vec<PathBuf>,
    /// The log panel is expanded (it folds away when another game opens).
    log_open: bool,
    /// Where the Run / Launch rows end (last frame), so the log never covers them.
    actions_bottom: Option<f32>,
    /// Run in RPCS3 was pressed but rpcs3.exe isn't set up.
    rpcs3_missing: bool,
    /// A newer AnyPS5 than the installed one (its tag).
    anyps5_update: Option<String>,
}

/// Something picked from a file's right-click menu.
enum RowAction {
    /// Convert a file inside the open package or game folder.
    ConvertInner(String),
    /// Convert a standalone file on disk.
    ConvertFile(PathBuf),
    Delete(PathBuf),
}

#[derive(Clone)]
struct BrowseRow {
    path: PathBuf,
    name: String,
    is_dir: bool,
    size: u64,
    /// A file the app can load (.pkg or an executable).
    openable: bool,
}

impl App {
    fn new() -> Self {
        let (tx, rx) = channel();
        App {
            path: None,
            passcode: std::env::var("PKGMASTER_PASSCODE").unwrap_or_default(),
            loaded: None,
            selected: None,
            busy: false,
            progress: None,
            status: Status::Info("Open a .pkg file, or drag one onto this window.".into()),
            tx,
            rx,
            settings: Settings::load(&settings_path()),
            confirm: None,
            run_dir: None,
            log: Vec::new(),
            game: Arc::new(Mutex::new(None)),
            browse: None,
            history: Arc::new(Mutex::new(History::load(&settings_path().with_file_name("history.tsv")))),
            show_history: false,
            revert_confirm: None,
            clear_confirm: false,
            delete_confirm: None,
            refresh_after_job: false,
            pending_refresh: false,
            icon_tex: None,
            shown_title: None,
            shadpkg_missing: false,
            anyps5_missing: false,
            is_ps4: false,
            cancel: Arc::new(AtomicBool::new(false)),
            cancellable: false,
            update: None,
            show_update: false,
            copied_at: None,
            library: Library::load(&settings_path().with_file_name("library.json")),
            library_tex: HashMap::new(),
            back_stack: Vec::new(),
            log_open: true,
            actions_bottom: None,
            rpcs3_missing: false,
            anyps5_update: None,
        }
    }

    fn passcode(&self) -> Option<String> {
        let p = self.passcode.trim();
        (!p.is_empty()).then(|| p.to_string())
    }

    fn start_load(&mut self, ctx: &egui::Context, path: PathBuf) {
        self.open(ctx, path, true);
    }

    /// Open a file or folder. A folder that holds a game (eboot) loads as a
    /// game. Otherwise, with `auto`, a lone .pkg is opened and a lone
    /// subfolder is stepped into; anything else is listed like the file view.
    /// Back and Refresh pass `auto = false` so they show the folder itself.
    fn open(&mut self, ctx: &egui::Context, path: PathBuf, auto: bool) {
        // Something new: remember what was open for Back, and fold the log away.
        if let Some(prev) = self.path.clone().filter(|p| *p != path) {
            if self.back_stack.last() != Some(&prev) {
                self.back_stack.push(prev);
            }
            self.log_open = false;
        }
        let mut path = path;
        while path.is_dir() && !has_eboot(&path) {
            let pkgs = pkgs_in_folder(&path);
            if auto && pkgs.len() == 1 {
                path = pkgs.into_iter().next().unwrap();
                break;
            }
            let subdirs = subfolders(&path);
            if auto && pkgs.is_empty() && subdirs.len() == 1 {
                path = subdirs.into_iter().next().unwrap();
                continue;
            }
            let rows = browse_rows(&path);
            let (dirs, files) = (rows.iter().filter(|r| r.is_dir).count(), rows.iter().filter(|r| !r.is_dir).count());
            self.status = Status::Info(format!(
                "{}: {dirs} folder(s), {files} file(s). Click a folder to open it, or a .pkg / eboot to load it.",
                file_label(&path)
            ));
            self.loaded = None;
            self.is_ps4 = false;
            self.icon_tex = None;
            self.selected = None;
            self.path = Some(path);
            self.browse = Some(rows);
            return;
        }
        self.browse = None;
        self.busy = true;
        self.loaded = None;
        self.is_ps4 = false;
        self.icon_tex = None;
        self.selected = None;
        self.progress = None;
        self.status = Status::Info(format!("Opening {}…", file_label(&path)));
        self.path = Some(path.clone());
        let (tx, ctx, passcode) = (self.tx.clone(), ctx.clone(), self.passcode());
        thread::spawn(move || {
            let result = load(&path, passcode.as_deref());
            let ps4 = result.as_ref().is_ok_and(looks_ps4);
            let _ = tx.send(Msg::Loaded(path, result, ps4));
            ctx.request_repaint();
        });
    }

    fn spawn_job<F>(&mut self, ctx: &egui::Context, job: F)
    where
        F: FnOnce(&dyn Fn(u64, u64, &str)) -> Result<String, Error> + Send + 'static,
    {
        self.busy = true;
        self.progress = Some((0, 0, String::new()));
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        thread::spawn(move || {
            let report = |done: u64, total: u64, item: &str| {
                let _ = tx.send(Msg::Progress(done, total, item.to_string()));
                ctx.request_repaint();
            };
            let result = job(&report);
            let _ = tx.send(Msg::Done(result));
            ctx.request_repaint();
        });
    }

    fn convert(&mut self, ctx: &egui::Context) {
        let Some(input) = self.loaded.as_ref().and_then(|l| l.input.clone()) else { return };
        let Some(base) = self.path.clone() else { return };
        let inner = self.selected.clone();
        self.convert_with(ctx, input, inner, &base);
    }

    /// Convert to ELF. The save dialog opens in a "<name>-elf" folder next to
    /// the game files (`base`), which is created for it.
    fn convert_with(&mut self, ctx: &egui::Context, input: Input, inner: Option<String>, base: &Path) {
        let source_name = match (&input, &inner) {
            (Input::Package(_) | Input::Folder(_), Some(p)) => p.clone(),
            _ => base.to_string_lossy().into_owned(),
        };
        let elf_dir = elf_folder_for(base);
        let created = !elf_dir.exists() && std::fs::create_dir_all(&elf_dir).is_ok();
        let mut dialog = rfd::FileDialog::new()
            .set_title("Save ELF as")
            .set_file_name(default_output_name(&source_name))
            .add_filter("ELF file", &["elf"]);
        if elf_dir.is_dir() {
            dialog = dialog.set_directory(&elf_dir);
        } else if let Some(dir) = base.parent() {
            dialog = dialog.set_directory(dir);
        }
        let out = dialog.save_file();
        // Don't leave an empty "-elf" folder behind if it wasn't used.
        if created && !out.as_ref().is_some_and(|o| o.starts_with(&elf_dir)) {
            let _ = std::fs::remove_dir(&elf_dir);
        }
        let Some(out) = out else { return };
        self.status = Status::Info("Converting…".into());
        let (hist, action) = (self.history.clone(), format!("Converted {source_name} to ELF"));
        self.spawn_job(ctx, move |progress| {
            let r = tracked(&hist, &action, &out, || convert_to_elf(&input, inner.as_deref(), &out, progress))?;
            let mut msg = format!(
                "Saved {} ({}) from {}: {} / {}, entry 0x{:X}.",
                file_label(&r.output),
                fmt_size(r.conversion.data.len() as u64),
                r.source,
                r.conversion.info.type_name(),
                r.conversion.info.machine_name(),
                r.conversion.info.e_entry
            );
            for w in &r.conversion.warnings {
                msg.push_str("\nNote: ");
                msg.push_str(w);
            }
            Ok(msg)
        });
    }

    fn extract(&mut self, ctx: &egui::Context) {
        let Some(Input::Package(pfs)) = self.loaded.as_ref().and_then(|l| l.input.clone()) else { return };
        let mut dialog = rfd::FileDialog::new().set_title("Extract files into");
        if let Some(dir) = self.path.as_ref().and_then(|p| p.parent()) {
            dialog = dialog.set_directory(dir);
        }
        let Some(dir) = dialog.pick_folder() else { return };
        let stem = self
            .path
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "package".into());
        let out = dir.join(stem);
        self.status = Status::Info("Extracting…".into());
        let (hist, action) = (self.history.clone(), format!("Extracted {}", out.display()));
        self.spawn_job(ctx, move |progress| {
            let n = tracked(&hist, &action, &out, || extract_all(&pfs, &out, progress))?;
            Ok(format!("Extracted {n} file(s) to {}.", out.display()))
        });
    }

    /// Ask for the AnyPS5 release folder (the one holding libs/) and, if no
    /// relinker is inside it, for the relinker; check and remember both.
    fn pick_anyps5(&mut self) -> bool {
        let mut dialog = rfd::FileDialog::new()
            .set_title("Pick your AnyPS5 folder (the one holding libs/, unpacked from prx-windows)");
        if let Some(d) = &self.settings.anyps5_dir {
            dialog = dialog.set_directory(d);
        }
        let Some(dir) = dialog.pick_folder() else { return false };
        if let Err(e) = find_libs(&dir) {
            self.status = Status::Err(describe_error(&e));
            return false;
        }
        let relinker = if find_relinker(&dir).is_some() {
            None
        } else {
            let mut dialog = rfd::FileDialog::new()
                .set_title("Pick the AnyPS5 relinker (relinker-v….exe from AnyPS5's releases)")
                .add_filter("Program", &["exe"])
                .add_filter("All files", &["*"]);
            let start = self.settings.relinker.as_ref().and_then(|r| r.parent()).or(dir.parent());
            if let Some(d) = start {
                dialog = dialog.set_directory(d);
            }
            let Some(file) = dialog.pick_file() else {
                self.status = Status::Err("No relinker picked; the AnyPS5 folder was not changed.".into());
                return false;
            };
            Some(file)
        };
        self.settings.anyps5_dir = Some(dir);
        self.settings.relinker = relinker;
        self.status = match self.settings.save(&settings_path()) {
            Ok(()) => Status::Ok("AnyPS5 folder saved.".into()),
            Err(e) => Status::Err(format!("AnyPS5 folder set, but it could not be saved: {}", describe_error(&e))),
        };
        true
    }

    fn run_anyps5(&mut self) {
        let Some(Input::Folder(root)) = self.loaded.as_ref().and_then(|l| l.input.clone()) else { return };
        if self.settings.release().is_err() {
            self.anyps5_missing = true;
            return;
        }
        let pre = match preflight(&root) {
            Ok(p) => p,
            Err(e) => {
                self.status = Status::Err(describe_error(&e));
                return;
            }
        };
        // The run folder goes straight into the AnyPS5 folder; no picker.
        let Some(anyps5_dir) = self.settings.anyps5_dir.clone() else { return };
        let name = root.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "game".into());
        self.confirm = Some((pre.warnings(), root, anyps5_dir.join(format!("{name}-run"))));
    }

    fn start_run(&mut self, ctx: &egui::Context, root: PathBuf, out: PathBuf) {
        let release = match self.settings.release() {
            Ok(r) => r,
            Err(e) => {
                self.status = Status::Err(describe_error(&e));
                return;
            }
        };
        let hist = self.history.clone();
        self.log.clear();
        self.run_dir = None;
        self.busy = true;
        self.progress = Some((0, 0, String::new()));
        self.status = Status::Info("Preparing and running the AnyPS5 relinker…".into());
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        thread::spawn(move || {
            let progress = |done: u64, total: u64, item: &str| {
                let _ = tx.send(Msg::Progress(done, total, item.to_string()));
                ctx.request_repaint();
            };
            let log = |line: &str| {
                let _ = tx.send(Msg::Log(line.to_string()));
                ctx.request_repaint();
            };
            let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let result = tracked(&hist, &format!("AnyPS5 run folder for {name}"), &out, || {
                run_relinker(&root, &release, &out, &progress, &log).map(|r| r.out_dir)
            });
            let _ = tx.send(Msg::Relinked(result));
            ctx.request_repaint();
        });
    }

    fn launch(&mut self, ctx: &egui::Context) {
        let Some(dir) = self.run_dir.clone() else { return };
        let child = match launch_command(&dir).and_then(spawn_piped) {
            Ok(c) => c,
            Err(e) => {
                self.status = Status::Err(describe_error(&e));
                return;
            }
        };
        self.log.push(format!("> {APP_EXE} (in {})", dir.display()));
        self.watch_game(ctx, child, APP_EXE.to_string());
    }

    /// Stream a started game's output into the log, keep it for Stop, and
    /// report when it exits.
    fn watch_game(&mut self, ctx: &egui::Context, mut child: Child, name: String) {
        self.log_open = true;
        let out = child.stdout.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>);
        let err = child.stderr.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>);
        for pipe in [out, err].into_iter().flatten() {
            let (tx, ctx) = (self.tx.clone(), ctx.clone());
            thread::spawn(move || {
                stream_lines(pipe, &|line: &str| {
                    let _ = tx.send(Msg::Log(line.to_string()));
                    ctx.request_repaint();
                })
            });
        }
        *self.game.lock().unwrap() = Some(child);
        self.status = Status::Info(format!("{name} is running."));
        let (game, tx, ctx) = (self.game.clone(), self.tx.clone(), ctx.clone());
        thread::spawn(move || loop {
            thread::sleep(Duration::from_millis(200));
            let mut guard = game.lock().unwrap();
            let Some(child) = guard.as_mut() else { break };
            let msg = match child.try_wait() {
                Ok(None) => continue,
                Ok(Some(status)) => format!("{name} exited ({status})."),
                Err(e) => format!("Lost track of {name}: {e}"),
            };
            *guard = None;
            let _ = tx.send(Msg::Exited(msg));
            ctx.request_repaint();
            break;
        });
    }

    /// Boot a dumped PS3 game in the user's own RPCS3, and keep it in the Library.
    fn run_rpcs3(&mut self, ctx: &egui::Context, game: &Path) {
        let Some(eboot) = ps3_game_dir(game).map(|d| d.join("USRDIR").join("EBOOT.BIN")) else {
            self.status = Status::Err(format!("{} isn't a PS3 game folder (no PARAM.SFO and USRDIR/EBOOT.BIN)", game.display()));
            return;
        };
        let Some(rpcs3) = find_rpcs3(&self.settings) else {
            self.rpcs3_missing = true;
            return;
        };
        let mut cmd = std::process::Command::new(&rpcs3);
        cmd.arg("--no-gui").arg(&eboot);
        if let Some(dir) = rpcs3.parent() {
            cmd.current_dir(dir);
        }
        let child = match spawn_piped(cmd) {
            Ok(c) => c,
            Err(e) => {
                self.status = Status::Err(format!("Could not start RPCS3: {}", describe_error(&e)));
                return;
            }
        };
        self.log.push(format!("> rpcs3 --no-gui {}", eboot.display()));
        if let Some(l) = self.loaded.as_ref().filter(|_| self.path.as_deref() == Some(game)) {
            self.library.add(game, &l.info, None);
            let _ = self.library.save();
        }
        self.watch_game(ctx, child, "RPCS3".to_string());
    }

    /// Start a Library game: a PS3 game boots in RPCS3, a game with an AnyPS5
    /// run folder starts its app.exe, and anything else just opens.
    fn boot(&mut self, ctx: &egui::Context, e: &Entry) {
        if !e.path.exists() {
            self.status = Status::Err(format!("{} isn't there any more.", e.path.display()));
            return;
        }
        self.start_load(ctx, e.path.clone());
        if self.game_running() {
            return;
        }
        if e.is_ps3() || ps3_game_dir(&e.path).is_some() {
            self.run_rpcs3(ctx, &e.path);
        } else if let Some(dir) = e.run_dir.clone().filter(|d| d.join(APP_EXE).is_file()) {
            self.run_dir = Some(dir);
            self.launch(ctx);
        }
    }

    /// Go back to what was open before.
    fn go_back(&mut self, ctx: &egui::Context) {
        if let Some(p) = self.back_stack.pop() {
            // Clearing the current path first keeps it off the Back list.
            self.path = None;
            self.open(ctx, p, false);
        }
    }

    fn stop_game(&mut self) {
        if let Some(child) = self.game.lock().unwrap().as_mut() {
            let _ = child.kill();
        }
    }

    fn game_running(&self) -> bool {
        self.game.lock().map(|g| g.is_some()).unwrap_or(false)
    }

    /// Runs shadPKG's `extract` on the open package (or one the user picks)
    /// into a chosen folder, with no console window.
    fn run_shadpkg(&mut self, ctx: &egui::Context) {
        let Some(exe) = find_shadpkg() else {
            // Not installed: explain where to get it and where to put it.
            self.shadpkg_missing = true;
            return;
        };
        let open_pkg = self
            .path
            .clone()
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("pkg")));
        let pkg = match open_pkg {
            Some(p) => p,
            None => {
                let mut dialog = rfd::FileDialog::new()
                    .set_title("Package to extract with shadPKG")
                    .add_filter("PS4/PS5 package", &PACKAGE_EXTS);
                if let Some(dir) = self.path.as_ref().and_then(|p| p.parent()) {
                    dialog = dialog.set_directory(dir);
                }
                let Some(p) = dialog.pick_file() else { return };
                p
            }
        };
        let Some(out) = rfd::FileDialog::new()
            .set_title("shadPKG output folder")
            .set_directory(pkg.parent().unwrap_or(Path::new(".")))
            .pick_folder()
        else {
            return;
        };
        // Each package gets its own folder, so the change can be reverted cleanly.
        let stem = pkg.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "package".into());
        let out = out.join(stem);
        let (hist, action) = (self.history.clone(), format!("shadPKG extracted {}", file_label(&pkg)));
        self.status = Status::Info(format!("Running shadPKG on {}…", file_label(&pkg)));
        self.spawn_job(ctx, move |progress| {
            tracked(&hist, &action, &out, || {
                std::fs::create_dir_all(&out)?;
                shadpkg_extract(&exe, &pkg, &out, progress)
            })
        });
    }

    fn handle_messages(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Loaded(path, result, ps4) => {
                    self.busy = false;
                    if self.path.as_deref() != Some(path.as_path()) {
                        continue;
                    }
                    match result {
                        Ok(l) => {
                            self.status = match &l.key_problem {
                                Some(Error::KeyRequired(_)) => {
                                    Status::Err("This package is encrypted.".into())
                                }
                                Some(e) => Status::Err(format!("That passcode didn't work: {e}")),
                                None => match &l.input {
                                    Some(Input::Package(_) | Input::Folder(_)) => Status::Ok(format!(
                                        "Opened {}. Pick a file below or use the defaults.",
                                        file_label(&path)
                                    )),
                                    _ => Status::Ok(format!("Opened {}.", file_label(&path))),
                                },
                            };
                            self.selected = l.default_elf.clone();
                            self.loaded = Some(l);
                            self.icon_tex = None;
                            self.is_ps4 = ps4;
                        }
                        Err(e) => self.status = Status::Err(describe_error(&e)),
                    }
                }
                Msg::Progress(done, total, item) => self.progress = Some((done, total, item)),
                Msg::Log(line) => {
                    self.log.push(line);
                    if self.log.len() > LOG_LIMIT {
                        self.log.drain(..self.log.len() - LOG_LIMIT);
                    }
                }
                Msg::Relinked(result) => {
                    self.busy = false;
                    self.progress = None;
                    self.status = match result {
                        Ok(dir) => {
                            let msg = format!("Ready in {}. Press Launch to start {APP_EXE}. Added to your Library.", dir.display());
                            // Running a game in AnyPS5 puts it in the Library, with its run folder.
                            if let (Some(game), Some(l)) = (self.path.clone(), self.loaded.as_ref()) {
                                self.library.add(&game, &l.info, Some(dir.clone()));
                                let _ = self.library.save();
                            }
                            self.run_dir = Some(dir);
                            Status::Ok(msg)
                        }
                        Err(e) => Status::Err(describe_error(&e)),
                    };
                }
                Msg::AnyPs5UpdateFound(tag) => self.anyps5_update = Some(tag),
                Msg::UpdateFound(release) => {
                    // Shown once per start, unless the user skipped this version.
                    self.show_update = self.settings.skip_version.as_deref() != Some(release.tag.as_str());
                    self.update = Some(release);
                }
                Msg::Updated(result) => {
                    self.busy = false;
                    self.cancellable = false;
                    self.progress = None;
                    match result {
                        Ok(exe) => match std::process::Command::new(&exe).spawn() {
                            Ok(_) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                            Err(e) => {
                                self.status = Status::Ok(format!("Updated. Start PKG Master again to use the new version ({e})."))
                            }
                        },
                        Err(e) => self.status = download_status(&e),
                    }
                }
                Msg::AnyPs5Installed(result) => {
                    self.busy = false;
                    self.cancellable = false;
                    self.progress = None;
                    self.status = match result {
                        Ok((dir, tag)) => {
                            self.settings.anyps5_dir = Some(dir.clone());
                            self.settings.relinker = None;
                            self.settings.anyps5_version = Some(tag);
                            self.anyps5_update = None;
                            match self.settings.save(&settings_path()) {
                                Ok(()) => Status::Ok(format!("AnyPS5 is set up in {}.", dir.display())),
                                Err(e) => Status::Err(format!(
                                    "AnyPS5 downloaded to {}, but the setting could not be saved: {}",
                                    dir.display(),
                                    describe_error(&e)
                                )),
                            }
                        }
                        Err(e) => download_status(&e),
                    };
                }
                Msg::Exited(m) => {
                    self.log.push(m.clone());
                    self.status = Status::Info(m);
                }
                Msg::Done(result) => {
                    if self.refresh_after_job {
                        self.refresh_after_job = false;
                        self.pending_refresh = true;
                    }
                    self.busy = false;
                    self.progress = None;
                    self.status = match result {
                        Ok(m) => Status::Ok(m),
                        Err(e) => Status::Err(describe_error(&e)),
                    };
                }
            }
        }
    }

    fn pick_file(&mut self, ctx: &egui::Context) {
        let mut dialog = rfd::FileDialog::new()
            .set_title("Open a package or executable")
            .add_filter("PS4/PS5 package", &PACKAGE_EXTS)
            .add_filter("SELF / ELF executable", &BINARY_EXTS)
            .add_filter("All files", &["*"]);
        if let Some(dir) = self.path.as_ref().and_then(|p| p.parent()) {
            dialog = dialog.set_directory(dir);
        }
        if let Some(path) = dialog.pick_file() {
            self.start_load(ctx, path);
        }
    }

    /// Upload the open game's icon and put its title in the title bar.
    fn update_game_header(&mut self, ctx: &egui::Context) {
        let info = self.loaded.as_ref().map(|l| &l.info);
        if self.icon_tex.is_none() {
            if let Some(icon) = info.and_then(|i| i.icon.as_ref()) {
                let image = egui::ColorImage::from_rgba_unmultiplied([icon.width as usize, icon.height as usize], &icon.rgba);
                self.icon_tex = Some(ctx.load_texture("game-icon", image, egui::TextureOptions::LINEAR));
            }
        }
        let title = info.and_then(|i| i.title.clone());
        if title != self.shown_title {
            let text = match &title {
                Some(t) => format!("PKG Master - {t}"),
                None => "PKG Master".to_string(),
            };
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(text));
            self.shown_title = title;
        }
    }

    fn apply_row_action(&mut self, ctx: &egui::Context, action: RowAction) {
        match action {
            RowAction::ConvertInner(p) => {
                self.selected = Some(p);
                self.convert(ctx);
            }
            RowAction::ConvertFile(p) => self.convert_with(ctx, Input::Binary(p.clone()), None, &p),
            RowAction::Delete(p) => self.delete_confirm = Some(p),
        }
    }

    /// Shown when Run shadPKG is pressed without shadPKG.exe installed.
    fn shadpkg_missing_ui(&mut self, ctx: &egui::Context) {
        if !self.shadpkg_missing {
            return;
        }
        let dir = shadpkg_dir();
        let (mut close, mut browse) = (false, false);
        egui::Window::new("shadPKG isn't installed")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_max_width(480.0);
                ui.label("Run shadPKG (PS4) needs shadPKG.exe, which isn't included with PKG Master.");
                ui.add_space(4.0);
                ui.label("1. Download shadPKG.exe from its official GitHub releases:");
                if ui.link(SHADPKG_RELEASES).clicked() {
                    open_url(SHADPKG_RELEASES);
                }
                ui.add_space(4.0);
                ui.label("2. Drop it in this folder:");
                ui.label(RichText::new(dir.display().to_string()).monospace());
                ui.add_space(4.0);
                ui.label("3. Press Run shadPKG (PS4) again.");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Open download page").clicked() {
                        open_url(SHADPKG_RELEASES);
                    }
                    if ui.button("Open shadpkg folder").clicked() {
                        let _ = std::fs::create_dir_all(&dir);
                        let _ = std::process::Command::new("explorer").arg(&dir).spawn();
                    }
                    browse = ui.button("I have it elsewhere…").on_hover_text("Pick your shadPKG.exe; it gets copied into the shadpkg folder").clicked();
                    close = ui.button("Close").clicked();
                });
            });
        if browse {
            if let Some(file) = rfd::FileDialog::new().set_title("Pick shadPKG.exe").add_filter("Program", &["exe"]).pick_file() {
                let dest = dir.join(SHADPKG_NAMES[0]);
                let copied = std::fs::create_dir_all(&dir).and_then(|_| std::fs::copy(&file, &dest));
                self.status = match copied {
                    Ok(_) => Status::Ok(format!("Copied shadPKG.exe into {}. Press Run shadPKG (PS4) again.", dir.display())),
                    Err(e) => Status::Err(format!("Could not copy it into {}: {e}", dir.display())),
                };
                close = true;
            }
        }
        if close {
            self.shadpkg_missing = false;
        }
    }

    /// An icon from the Library's icon cache, uploaded once.
    fn library_icon(&mut self, ctx: &egui::Context, icon: &Path) -> Option<egui::TextureHandle> {
        self.library_tex
            .entry(icon.to_path_buf())
            .or_insert_with(|| {
                let img = image::open(icon).ok()?.to_rgba8();
                let size = [img.width() as usize, img.height() as usize];
                let image = egui::ColorImage::from_rgba_unmultiplied(size, &img);
                Some(ctx.load_texture(icon.display().to_string(), image, egui::TextureOptions::LINEAR))
            })
            .clone()
    }

    /// The Library dropdown: the saved games as tiles, three across, two
    /// rows visible at a time. Returns what was picked.
    fn library_menu(&mut self, ui: &mut egui::Ui) -> Option<LibraryAction> {
        const TILE: egui::Vec2 = egui::vec2(150.0, 150.0);
        const ICON: f32 = 100.0;
        let mut action = None;
        // A fixed-size rectangle: three tiles across, two rows, scroll for more.
        let size = egui::vec2(3.0 * TILE.x + 2.0 * 8.0 + 16.0, 2.0 * TILE.y + 8.0);
        ui.set_min_size(size);
        ui.set_max_width(size.x);
        if self.library.entries.is_empty() {
            ui.add_space(8.0);
            ui.label("Your Library is empty.");
            ui.weak("Press the star next to a game's title to save it here. Games you run in AnyPS5 or RPCS3 are added for you.");
            ui.add_space(8.0);
            return None;
        }
        let entries = self.library.entries.clone();
        egui::ScrollArea::vertical().max_height(size.y).auto_shrink([false, false]).show(ui, |ui| {
            egui::Grid::new("library-grid").spacing([8.0, 8.0]).show(ui, |ui| {
                for (i, e) in entries.iter().enumerate() {
                    let tex = e.icon.as_ref().and_then(|p| self.library_icon(ui.ctx(), p));
                    let (rect, resp) = ui.allocate_exact_size(TILE, egui::Sense::click());
                    let fill = ui.style().interact(&resp).weak_bg_fill;
                    ui.painter().rect_filled(rect, 6.0, fill);
                    let icon_rect = egui::Rect::from_center_size(
                        rect.center_top() + egui::vec2(0.0, 8.0 + ICON / 2.0),
                        egui::vec2(ICON, ICON),
                    );
                    match &tex {
                        Some(t) => egui::Image::new(t).paint_at(ui, icon_rect),
                        None => {
                            ui.painter().rect_filled(icon_rect, 6.0, ui.visuals().faint_bg_color);
                        }
                    }
                    let missing = !e.path.exists();
                    let color = if missing { ui.visuals().weak_text_color() } else { ui.visuals().text_color() };
                    let galley = ui.painter().layout(e.title.clone(), egui::FontId::proportional(13.0), color, TILE.x - 12.0);
                    let text_pos = egui::pos2(rect.center().x - galley.size().x / 2.0, icon_rect.bottom() + 6.0);
                    ui.painter().with_clip_rect(rect).galley(text_pos, galley, color);
                    let what = if e.is_ps3() {
                        "Click to play in RPCS3"
                    } else if e.run_dir.as_ref().is_some_and(|d| d.join(APP_EXE).is_file()) {
                        "Click to play (AnyPS5)"
                    } else {
                        "Click to open"
                    };
                    let resp = resp.on_hover_text(if missing {
                        format!("{}\nNot found any more", e.path.display())
                    } else {
                        format!("{what}\n{}", e.path.display())
                    });
                    if resp.clicked() && !missing {
                        action = Some(LibraryAction::Boot(e.clone()));
                        ui.close_menu();
                    }
                    resp.context_menu(|ui| {
                        if ui.add_enabled(!missing, egui::Button::new("Open game files")).clicked() {
                            action = Some(LibraryAction::Open(e.path.clone()));
                            ui.close_menu();
                        }
                        if ui.button("Show in Explorer").clicked() {
                            reveal(&e.path);
                            ui.close_menu();
                        }
                        if ui.button("Remove from Library").clicked() {
                            action = Some(LibraryAction::Remove(e.path.clone()));
                            ui.close_menu();
                        }
                    });
                    if i % 3 == 2 {
                        ui.end_row();
                    }
                }
            });
        });
        action
    }

    /// Star or unstar the open game.
    fn toggle_library(&mut self) {
        let (Some(path), Some(l)) = (self.path.clone(), self.loaded.as_ref()) else { return };
        if self.library.contains(&path) {
            self.library.remove(&path);
            self.status = Status::Info("Removed from your Library.".into());
        } else {
            self.library.add(&path, &l.info, None);
            self.status = Status::Ok("Saved to your Library.".into());
        }
        if let Err(e) = self.library.save() {
            self.status = Status::Err(format!("The Library could not be saved: {}", describe_error(&e)));
        }
    }

    /// Run a download on a worker thread with progress and a Cancel button;
    /// `job` returns the message to send when it ends.
    fn spawn_download<F>(&mut self, ctx: &egui::Context, job: F)
    where
        F: FnOnce(&dyn Fn(u64, u64, &str), &AtomicBool) -> Msg + Send + 'static,
    {
        self.busy = true;
        self.cancellable = true;
        self.cancel.store(false, Ordering::Relaxed);
        self.progress = Some((0, 0, String::new()));
        let (tx, ctx, cancel) = (self.tx.clone(), ctx.clone(), self.cancel.clone());
        thread::spawn(move || {
            let progress = |done: u64, total: u64, item: &str| {
                let _ = tx.send(Msg::Progress(done, total, item.to_string()));
                ctx.request_repaint();
            };
            let _ = tx.send(job(&progress, &cancel));
            ctx.request_repaint();
        });
    }

    /// Look for a newer PKG Master in the background (skipped when
    /// `PKGMASTER_NO_UPDATE_CHECK` is set). A failed check is silent.
    fn check_for_update(&self, ctx: &egui::Context) {
        if std::env::var_os("PKGMASTER_NO_UPDATE_CHECK").is_some() {
            return;
        }
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        let anyps5 = self.settings.installed_anyps5_version();
        thread::spawn(move || {
            if let Ok(Some(release)) = update::check() {
                let _ = tx.send(Msg::UpdateFound(release));
                ctx.request_repaint();
            }
            // AnyPS5 changes often; say when a newer one is out.
            if let Some(installed) = anyps5 {
                if let Ok(r) = pkgmaster::download::latest_release(ANYPS5_REPO) {
                    if update::is_newer(&r.tag, &installed) {
                        let _ = tx.send(Msg::AnyPs5UpdateFound(r.tag));
                        ctx.request_repaint();
                    }
                }
            }
        });
    }

    /// Download the newer AnyPS5 into a new folder beside the current one,
    /// then switch to it. The old folder (and its run folders) stays.
    fn update_anyps5(&mut self, ctx: &egui::Context) {
        let Some(tag) = self.anyps5_update.clone() else { return };
        let base = self
            .settings
            .anyps5_dir
            .as_ref()
            .and_then(|d| d.parent())
            .filter(|p| can_write(p))
            .map(Path::to_path_buf)
            .or_else(|| anyps5_download_dir().parent().map(Path::to_path_buf))
            .unwrap_or_default();
        let dest = base.join(format!("AnyPS5-{tag}"));
        self.status = Status::Info(format!("Downloading AnyPS5 {tag}…"));
        self.spawn_download(ctx, move |progress, cancel| Msg::AnyPs5Installed(install_release(&dest, progress, cancel)));
    }

    /// Shown when Run in RPCS3 is pressed without RPCS3 set up.
    fn rpcs3_missing_ui(&mut self, ctx: &egui::Context) {
        if !self.rpcs3_missing {
            return;
        }
        let (mut close, mut pick) = (false, false);
        egui::Window::new("RPCS3 isn't set up")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_max_width(480.0);
                ui.label("Run in RPCS3 plays PS3 games with RPCS3, the PS3 emulator. It isn't included with PKG Master.");
                ui.add_space(4.0);
                ui.label("1. Download RPCS3 from its official site and unzip it:");
                if ui.link(RPCS3_DOWNLOAD).clicked() {
                    open_url(RPCS3_DOWNLOAD);
                }
                ui.label("2. Start it once and install the PS3 system software, as its quickstart guide explains.");
                ui.label("3. Press \"I have it, pick rpcs3.exe…\" below.");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    pick = ui.button("I have it, pick rpcs3.exe…").clicked();
                    if ui.button("Open download page").clicked() {
                        open_url(RPCS3_DOWNLOAD);
                    }
                    close = ui.button("Close").clicked();
                });
            });
        if pick {
            if let Some(file) = rfd::FileDialog::new().set_title("Pick rpcs3.exe").add_filter("Program", &["exe"]).pick_file() {
                self.settings.rpcs3 = Some(file);
                self.status = match self.settings.save(&settings_path()) {
                    Ok(()) => Status::Ok("RPCS3 saved. Press Run in RPCS3 again.".into()),
                    Err(e) => Status::Err(format!("RPCS3 set, but it could not be saved: {}", describe_error(&e))),
                };
                close = true;
            }
        }
        if close {
            self.rpcs3_missing = false;
        }
    }

    /// "PKG Master vX is out", with Update now.
    fn update_ui(&mut self, ctx: &egui::Context) {
        if !self.show_update {
            return;
        }
        let Some(release) = self.update.clone() else { return };
        let (mut now, mut later, mut skip) = (false, false, false);
        egui::Window::new("Update available")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_max_width(480.0);
                ui.label(
                    RichText::new(format!("PKG Master {} is out. You have v{}.", release.tag, update::current_version()))
                        .size(16.0)
                        .strong(),
                );
                if !release.notes.trim().is_empty() {
                    ui.add_space(6.0);
                    egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                        for line in update::plain_notes(&release.notes) {
                            match line {
                                update::NoteLine::Heading(t) => {
                                    ui.add_space(4.0);
                                    ui.strong(t);
                                }
                                update::NoteLine::Bullet(t) => {
                                    ui.label(format!("•  {t}"));
                                }
                                update::NoteLine::Text(t) => {
                                    ui.label(t);
                                }
                            }
                        }
                    });
                }
                ui.add_space(6.0);
                ui.weak("Update now downloads it from GitHub, replaces this program and restarts it. Your settings, history, shadpkg and AnyPS5 folders stay as they are.");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    now = ui.add_enabled(!self.busy, egui::Button::new(RichText::new("Update now").strong())).clicked();
                    if ui.button("Open release page").clicked() {
                        open_url(if release.page.is_empty() { update::RELEASES } else { &release.page });
                    }
                    later = ui.button("Later").clicked();
                    skip = ui.button("Skip this version").clicked();
                });
            });
        if now {
            self.status = Status::Info(format!("Downloading PKG Master {}…", release.tag));
            self.spawn_download(ctx, move |progress, cancel| Msg::Updated(update::install(&release, progress, cancel)));
            self.show_update = false;
        } else if skip {
            self.settings.skip_version = Some(release.tag.clone());
            let _ = self.settings.save(&settings_path());
            self.update = None;
            self.show_update = false;
        } else if later {
            self.show_update = false;
        }
    }

    /// Shown when an AnyPS5 button is pressed without AnyPS5 set up.
    fn anyps5_missing_ui(&mut self, ctx: &egui::Context) {
        if !self.anyps5_missing {
            return;
        }
        let dest = anyps5_download_dir();
        let (mut close, mut pick, mut download) = (false, false, false);
        egui::Window::new("AnyPS5 isn't installed")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_max_width(500.0);
                ui.label("Running PS5 games needs AnyPS5, which isn't included with PKG Master.");
                ui.add_space(4.0);
                ui.label("1. From AnyPS5's GitHub releases, download prx-windows-v….zip and relinker-v….exe:");
                if ui.link(ANYPS5_RELEASES).clicked() {
                    open_url(ANYPS5_RELEASES);
                }
                ui.add_space(4.0);
                ui.label("2. Unzip prx-windows-v….zip (the folder with libs/ in it).");
                ui.label("3. Drop relinker-v….exe into that folder.");
                ui.label("4. Press \"I have it, pick the folder…\" and choose that folder.");
                ui.add_space(8.0);
                ui.label(RichText::new("Or let PKG Master do all of that:").strong());
                ui.weak(format!("Downloads the official release (about 90 MB) into {}", dest.display()));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    download = ui.add_enabled(!self.busy, egui::Button::new("Download for me")).clicked();
                    pick = ui.add_enabled(!self.busy, egui::Button::new("I have it, pick the folder…")).clicked();
                    if ui.button("Open download page").clicked() {
                        open_url(ANYPS5_RELEASES);
                    }
                    close = ui.button("Close").clicked();
                });
            });
        if pick && self.pick_anyps5() {
            close = true;
        }
        if download {
            self.status = Status::Info("Downloading AnyPS5 from its GitHub releases…".into());
            self.spawn_download(ctx, move |progress, cancel| Msg::AnyPs5Installed(install_release(&dest, progress, cancel)));
            close = true;
        }
        if close {
            self.anyps5_missing = false;
        }
    }

    /// Confirmation before moving a file or folder to the Recycle Bin.
    fn delete_ui(&mut self, ctx: &egui::Context) {
        let Some(p) = self.delete_confirm.clone() else { return };
        let mut decision = None;
        egui::Window::new("Move to the Recycle Bin?")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_max_width(460.0);
                ui.label(p.display().to_string());
                ui.weak("You can restore it from the Recycle Bin.");
                ui.horizontal(|ui| {
                    if ui.button("Delete").clicked() {
                        decision = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        decision = Some(false);
                    }
                });
            });
        match decision {
            Some(true) => {
                self.delete_confirm = None;
                self.refresh_after_job = true;
                self.status = Status::Info(format!("Moving {} to the Recycle Bin…", file_label(&p)));
                self.spawn_job(ctx, move |_| {
                    trash::delete(&p).map_err(|e| {
                        Error::Conversion(format!("could not move {} to the Recycle Bin: {e}", p.display()))
                    })?;
                    Ok(format!("Moved {} to the Recycle Bin.", p.display()))
                });
            }
            Some(false) => self.delete_confirm = None,
            None => {}
        }
    }

    /// The History side panel and its Revert / Clear confirmations.
    fn history_ui(&mut self, ctx: &egui::Context) {
        if self.show_history {
            let (mut revert, mut clear, mut show, mut close) = (None, false, None, false);
            egui::SidePanel::right("history").resizable(true).default_width(360.0).show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("History");
                    close = ui.small_button("Close").clicked();
                });
                ui.weak("Everything PKG Master wrote. Revert deletes it and puts back whatever was there before.");
                ui.separator();
                let Ok(h) = self.history.try_lock() else {
                    ui.weak("Busy…");
                    return;
                };
                if h.changes.is_empty() {
                    ui.weak("Nothing yet.");
                }
                egui::ScrollArea::vertical().auto_shrink([false, false]).max_height(ui.available_height() - 36.0).show(
                    ui,
                    |ui| {
                        for c in h.changes.iter().rev() {
                            ui.group(|ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    ui.strong(&c.action);
                                    ui.weak(ago(c.time));
                                });
                                ui.label(RichText::new(c.target.display().to_string()).monospace().size(11.0));
                                if c.backup.is_some() {
                                    ui.weak("Replaced something that was already there (kept a backup).");
                                }
                                ui.horizontal(|ui| {
                                    if c.reverted {
                                        ui.weak("Reverted");
                                    } else {
                                        if ui.add_enabled(!self.busy, egui::Button::new("Revert")).clicked() {
                                            revert = Some((c.id, c.target.clone(), c.backup.is_some()));
                                        }
                                        if ui.button("Show in Explorer").clicked() {
                                            show = Some(c.target.clone());
                                        }
                                    }
                                });
                            });
                        }
                    },
                );
                clear = ui
                    .add_enabled(!self.busy && !h.changes.is_empty(), egui::Button::new("Clear history…"))
                    .on_hover_text("Forget everything listed here and delete the backups. Your files stay as they are.")
                    .clicked();
            });
            if close {
                self.show_history = false;
            }
            if revert.is_some() {
                self.revert_confirm = revert;
            }
            if clear {
                self.clear_confirm = true;
            }
            if let Some(p) = show {
                reveal(&p);
            }
        }

        if let Some((id, target, had_backup)) = self.revert_confirm.clone() {
            let mut decision = None;
            egui::Window::new("Revert this change?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.set_max_width(460.0);
                    ui.label(format!("This deletes {}", target.display()));
                    if had_backup {
                        ui.label("and puts back the version that was there before.");
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Revert").clicked() {
                            decision = Some(true);
                        }
                        if ui.button("Cancel").clicked() {
                            decision = Some(false);
                        }
                    });
                });
            match decision {
                Some(true) => {
                    self.revert_confirm = None;
                    let hist = self.history.clone();
                    self.status = Status::Info("Reverting…".into());
                    self.spawn_job(ctx, move |_| hist.lock().unwrap_or_else(|e| e.into_inner()).revert(id));
                }
                Some(false) => self.revert_confirm = None,
                None => {}
            }
        }

        if self.clear_confirm {
            let mut decision = None;
            egui::Window::new("Clear history?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.set_max_width(460.0);
                    ui.label("This forgets every change and deletes the backups, so nothing listed can be reverted afterwards.");
                    ui.label("Your files stay exactly as they are now.");
                    ui.horizontal(|ui| {
                        if ui.button("Clear").clicked() {
                            decision = Some(true);
                        }
                        if ui.button("Cancel").clicked() {
                            decision = Some(false);
                        }
                    });
                });
            match decision {
                Some(true) => {
                    self.clear_confirm = false;
                    let hist = self.history.clone();
                    self.spawn_job(ctx, move |_| {
                        hist.lock().unwrap_or_else(|e| e.into_inner()).clear()?;
                        Ok("History cleared and backups deleted.".into())
                    });
                }
                Some(false) => self.clear_confirm = false,
                None => {}
            }
        }
    }

    fn pick_folder(&mut self, ctx: &egui::Context) {
        let mut dialog = rfd::FileDialog::new().set_title("Open a decrypted game folder (the one holding eboot.bin)");
        if let Some(dir) = self.path.as_ref().and_then(|p| p.parent()) {
            dialog = dialog.set_directory(dir);
        }
        if let Some(path) = dialog.pick_folder() {
            self.start_load(ctx, path);
        }
    }
}

/// A folder that is itself a game: a PS4/PS5 dump (eboot) or a PS3 dump.
fn has_eboot(dir: &Path) -> bool {
    ["eboot.bin", "eboot.elf"].iter().any(|e| dir.join(e).is_file()) || ps3_game_dir(dir).is_some()
}

fn subfolders(dir: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_dir()).collect())
        .unwrap_or_default();
    dirs.sort();
    dirs
}

/// A folder's contents for the listing: folders first, then files.
fn browse_rows(dir: &Path) -> Vec<BrowseRow> {
    let mut rows: Vec<BrowseRow> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| {
                    let path = e.path();
                    let is_dir = path.is_dir();
                    let ext = path.extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
                    BrowseRow {
                        name: e.file_name().to_string_lossy().into_owned(),
                        size: if is_dir { 0 } else { e.metadata().map(|m| m.len()).unwrap_or(0) },
                        openable: !is_dir && (PACKAGE_EXTS.contains(&ext.as_str()) || BINARY_EXTS.contains(&ext.as_str())),
                        is_dir,
                        path,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    rows.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    rows
}

/// Run `op`, which writes `target`, and record it in the history. An
/// existing target is backed up first; if `op` fails, nothing changes.
fn tracked<T>(
    hist: &Arc<Mutex<History>>,
    action: &str,
    target: &Path,
    op: impl FnOnce() -> Result<T, Error>,
) -> Result<T, Error> {
    let pending = hist.lock().unwrap_or_else(|e| e.into_inner()).begin(action, target)?;
    let result = op();
    hist.lock().unwrap_or_else(|e| e.into_inner()).finish(pending, result.is_ok())?;
    result
}

/// Where Convert to ELF saves by default: "<name>-elf" next to `base` (the
/// opened package, game folder or file).
fn elf_folder_for(base: &Path) -> PathBuf {
    let parent = base.parent().unwrap_or(Path::new("."));
    let name = if base.is_dir() { base.file_name() } else { base.file_stem() };
    let name = name.map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "game".into());
    parent.join(format!("{name}-elf"))
}

fn is_pkg_path(p: &Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("pkg"))
}

/// The right-click options shared by every real file or folder on disk.
fn file_menu(ui: &mut egui::Ui, full: &Path, action: &mut Option<RowAction>) {
    if ui.button("Show in Explorer").clicked() {
        reveal(full);
        ui.close_menu();
    }
    if ui.button("Copy path").clicked() {
        ui.ctx().copy_text(full.display().to_string());
        ui.close_menu();
    }
    ui.separator();
    if ui.button("Delete (to Recycle Bin)…").clicked() {
        *action = Some(RowAction::Delete(full.to_path_buf()));
        ui.close_menu();
    }
}

/// Show a file or folder in File Explorer (its folder, if it is gone).
fn reveal(p: &Path) {
    let _ = if p.exists() {
        std::process::Command::new("explorer").arg(format!("/select,{}", p.display())).spawn()
    } else {
        std::process::Command::new("explorer").arg(p.parent().unwrap_or(p)).spawn()
    };
}

/// The `.pkg` files directly inside `dir`, when `dir` is a folder without an
/// eboot (so it is not a decrypted game folder). Empty otherwise.
fn pkgs_in_folder(dir: &Path) -> Vec<PathBuf> {
    if !dir.is_dir() || has_eboot(dir) {
        return Vec::new();
    }
    let mut pkgs: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_file() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("pkg")))
                .collect()
        })
        .unwrap_or_default();
    pkgs.sort();
    pkgs
}

fn file_label(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
}

/// shadPKG's official releases, where users download shadPKG.exe themselves.
const SHADPKG_RELEASES: &str = "https://github.com/seregonwar/ShadPKG/releases";
/// The folder next to pkgmaster-gui.exe where shadPKG.exe goes.
const SHADPKG_DIR: &str = "shadpkg";
const SHADPKG_NAMES: [&str; 2] = ["shadPKG.exe", "shadpkg.exe"];

/// `shadpkg\` next to this app: the place to drop shadPKG.exe.
fn shadpkg_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join(SHADPKG_DIR)))
        .unwrap_or_else(|| PathBuf::from(SHADPKG_DIR))
}

/// Looks for shadPKG.exe: `PKGMASTER_SHADPKG`, then in the `shadpkg\` folder
/// next to this app, then next to (or in a `shadpkg\` or `shad\` folder
/// beside) this app or any of its parent folders.
fn find_shadpkg() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("PKGMASTER_SHADPKG").map(PathBuf::from) {
        if p.is_file() {
            return Some(p);
        }
    }
    let exe = std::env::current_exe().ok()?;
    for dir in exe.ancestors().skip(1) {
        for sub in [Path::new(SHADPKG_DIR), Path::new("shad"), Path::new("")] {
            for name in SHADPKG_NAMES {
                let candidate = dir.join(sub).join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

const RPCS3_DOWNLOAD: &str = "https://rpcs3.net/download";

/// The user's rpcs3.exe: the one they picked, `PKGMASTER_RPCS3`, or an
/// `rpcs3\rpcs3.exe` next to this app.
fn find_rpcs3(settings: &Settings) -> Option<PathBuf> {
    let beside_app = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("rpcs3").join("rpcs3.exe")));
    [settings.rpcs3.clone(), std::env::var_os("PKGMASTER_RPCS3").map(PathBuf::from), beside_app]
        .into_iter()
        .flatten()
        .find(|p| p.is_file())
}

/// Something picked in the Library dropdown.
enum LibraryAction {
    Boot(Entry),
    Open(PathBuf),
    Remove(PathBuf),
}

/// Where "Download for me" puts AnyPS5: an `AnyPS5\` folder next to this
/// app, or `%LOCALAPPDATA%\PKG Master\AnyPS5` when the app's folder can't be
/// written to (such as Program Files). Worked out once.
fn anyps5_download_dir() -> PathBuf {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let beside_app = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("AnyPS5")));
        if let Some(dir) = beside_app.filter(|d| d.parent().is_some_and(can_write)) {
            return dir;
        }
        std::env::var_os("LOCALAPPDATA")
            .map(|d| Path::new(&d).join("PKG Master").join("AnyPS5"))
            .unwrap_or_else(|| PathBuf::from("AnyPS5"))
    })
    .clone()
}

/// Whether a file can be created in `dir`.
fn can_write(dir: &Path) -> bool {
    let probe = dir.join(format!(".pkgmaster-write-test-{}", std::process::id()));
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// The status line for a download that ended with `e`: a cancel isn't an error.
fn download_status(e: &Error) -> Status {
    match e {
        Error::Conversion(m) if m == CANCELLED => Status::Info(CANCELLED.into()),
        other => Status::Err(describe_error(other)),
    }
}

/// A PS4 game: a CUSA title ID, or, without a title ID, a PS4 package or a
/// PS4 SELF executable. The title ID wins because some PS5 (PPSA) games
/// ship a few PS4-format modules.
fn looks_ps4(l: &Loaded) -> bool {
    if let Some(id) = &l.info.title_id {
        return id.to_uppercase().starts_with("CUSA");
    }
    if l.rows.iter().any(|(k, v)| k == "Format" && v.starts_with("PS4")) {
        return true;
    }
    match &l.input {
        Some(Input::Folder(root)) => preflight(root).is_ok_and(|p| p.ps4.is_some()),
        Some(Input::Binary(p)) => is_ps4_self(p),
        _ => false,
    }
}

/// Open a web page in the default browser.
fn open_url(url: &str) {
    let _ = std::process::Command::new("explorer").arg(url).spawn();
}

fn shadpkg_extract(exe: &Path, pkg: &Path, out: &Path, progress: &dyn Fn(u64, u64, &str)) -> Result<String, Error> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    let mut cmd = Command::new(exe);
    cmd.arg("extract")
        .arg(pkg)
        .arg(out)
        // shadPKG writes its log into the working directory.
        .current_dir(out)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn()?;

    let mut stderr = child.stderr.take().expect("piped stderr");
    let err_reader = thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });

    // Show each stdout line (shadPKG redraws progress with '\r') as it arrives.
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut lines: Vec<String> = Vec::new();
    let mut current = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = stdout.read(&mut buf)?;
        if n == 0 {
            break;
        }
        for &b in &buf[..n] {
            if b == b'\n' || b == b'\r' {
                let line = String::from_utf8_lossy(&current).trim().to_string();
                current.clear();
                if !line.is_empty() {
                    progress(0, 0, &line);
                    lines.push(line);
                }
            } else {
                current.push(b);
            }
        }
    }
    let tail = String::from_utf8_lossy(&current).trim().to_string();
    if !tail.is_empty() {
        lines.push(tail);
    }

    let status = child.wait()?;
    let err_text = err_reader.join().unwrap_or_default();
    let last: Vec<&str> = lines.iter().rev().take(3).rev().map(String::as_str).collect();
    if status.success() {
        Ok(format!("shadPKG extracted {} to {}.\n{}", file_label(pkg), out.display(), last.join("\n")))
    } else {
        let detail = if err_text.trim().is_empty() { last.join("\n") } else { err_text.trim().to_string() };
        Err(Error::Conversion(format!("shadPKG failed ({status}):\n{detail}")))
    }
}

fn describe_error(e: &Error) -> String {
    match e {
        Error::Unsupported(m) => format!("Not supported: {m}"),
        Error::NotAPackage(m) => format!("Not a package: {m}"),
        Error::Io(err) => format!("File error: {err}"),
        other => other.to_string(),
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_messages(ctx);
        self.update_game_header(ctx);
        if std::mem::take(&mut self.pending_refresh) {
            if let Some(p) = self.path.clone() {
                self.open(ctx, p, false);
            }
        }

        let dropped = ctx.input(|i| i.raw.dropped_files.first().and_then(|f| f.path.clone()));
        if let Some(path) = dropped {
            if !self.busy {
                self.start_load(ctx, path);
            }
        }

        let status_top = egui::TopBottomPanel::bottom("status")
            .show(ctx, |ui| {
                ui.add_space(6.0);
                if let Some((done, total, item)) = &self.progress {
                    let frac = if *total > 0 { *done as f32 / *total as f32 } else { 0.0 };
                    ui.horizontal(|ui| {
                        if self.cancellable && self.busy && ui.button("Cancel").clicked() {
                            self.cancel.store(true, Ordering::Relaxed);
                        }
                        ui.add(egui::ProgressBar::new(frac).show_percentage().text(item.as_str()));
                    });
                }
                let (text, color) = match &self.status {
                    Status::Info(m) => (m.as_str(), ui.visuals().text_color()),
                    Status::Ok(m) => (m.as_str(), Color32::from_rgb(0x2E, 0xA0, 0x43)),
                    Status::Err(m) => (m.as_str(), Color32::from_rgb(0xD0, 0x3B, 0x3B)),
                };
                ui.label(RichText::new(text).color(color));
                ui.add_space(6.0);
            })
            .response
            .rect
            .top();

        if let Some((warnings, root, out)) = self.confirm.clone() {
            let mut decision = None;
            egui::Window::new("Run in AnyPS5")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.set_max_width(460.0);
                    for w in &warnings {
                        ui.label(RichText::new(format!("⚠ {w}")).color(Color32::from_rgb(0xC8, 0x8A, 0x10)));
                        ui.add_space(4.0);
                    }
                    ui.label(format!("The run folder will be {}.", out.display()));
                    ui.weak("The game folder itself is only read, never changed.");
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button(if warnings.is_empty() { "Start" } else { "Continue anyway" }).clicked() {
                            decision = Some(true);
                        }
                        if ui.button("Cancel").clicked() {
                            decision = Some(false);
                        }
                    });
                });
            match decision {
                Some(true) => {
                    self.confirm = None;
                    self.start_run(ctx, root, out);
                }
                Some(false) => self.confirm = None,
                None => {}
            }
        }

        if !self.log.is_empty() {
            // The log can be dragged taller, but never over the Run / Launch rows.
            let room = match self.actions_bottom {
                Some(bottom) => status_top - bottom - 6.0,
                None => ctx.screen_rect().height() * 0.6,
            };
            let header = |ui: &mut egui::Ui, app: &mut App| {
                ui.horizontal(|ui| {
                    ui.strong("Log");
                    let toggle = if app.log_open { "Hide".to_string() } else { format!("Show ({} lines)", app.log.len()) };
                    if ui.small_button(toggle).clicked() {
                        app.log_open = !app.log_open;
                    }
                    if ui.small_button("Clear").clicked() {
                        app.log.clear();
                    }
                    // The button says "Copied" for a moment; the status line is left alone.
                    let now = ui.ctx().input(|i| i.time);
                    let copied = app.copied_at.is_some_and(|t| now - t < 1.5);
                    if ui.small_button(if copied { "Copied!" } else { "Copy log" }).on_hover_text("Copy every line to the clipboard").clicked() {
                        ui.ctx().copy_text(app.log.join("\n"));
                        app.copied_at = Some(now);
                        ui.ctx().request_repaint_after(Duration::from_millis(1600));
                    }
                });
            };
            if self.log_open {
                egui::TopBottomPanel::bottom("log")
                    .resizable(true)
                    .default_height(160.0_f32.min(room.max(60.0)))
                    .min_height(60.0)
                    .max_height(room.max(60.0))
                    .show(ctx, |ui| {
                        header(ui, self);
                        egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
                            for line in &self.log {
                                ui.label(RichText::new(line).monospace().size(12.0));
                            }
                        });
                    });
            } else {
                egui::TopBottomPanel::bottom("log-folded").show(ctx, |ui| header(ui, self));
            }
        }

        // The toolbar spans the full width, above the History panel.
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.add_space(4.0);
            // A slim row while a newer version is waiting.
            if let Some(tag) = self.update.as_ref().map(|r| r.tag.clone()) {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("PKG Master {tag} is out.")).color(Color32::from_rgb(0x2E, 0xA0, 0x43)));
                    if ui.add_enabled(!self.busy, egui::Button::new("Update…").small()).clicked() {
                        self.show_update = true;
                    }
                });
                ui.add_space(2.0);
            }
            ui.horizontal(|ui| {
                if ui.add_enabled(!self.busy, egui::Button::new(RichText::new("Open file…").size(16.0))).clicked() {
                    self.pick_file(ctx);
                }
                if ui.add_enabled(!self.busy, egui::Button::new(RichText::new("Open folder…").size(16.0))).clicked() {
                    self.pick_folder(ctx);
                }
                let mut picked = None;
                ui.add_enabled_ui(!self.busy, |ui| {
                    ui.menu_button(RichText::new(format!("{ICON_LIBRARY} Library")).size(16.0), |ui| {
                        picked = self.library_menu(ui);
                    });
                });
                match picked {
                    Some(LibraryAction::Boot(e)) => self.boot(ctx, &e),
                    Some(LibraryAction::Open(p)) => self.start_load(ctx, p),
                    Some(LibraryAction::Remove(p)) => {
                        self.library.remove(&p);
                        let _ = self.library.save();
                    }
                    None => {}
                }
                // shadPKG only makes sense for an open PS4 package.
                let ps4_pkg = self.is_ps4 && self.path.as_deref().is_some_and(is_pkg_path);
                if ps4_pkg
                    && ui
                        .add_enabled(!self.busy, egui::Button::new(RichText::new("Run shadPKG (PS4)…").size(16.0)))
                        .on_hover_text("Extract this PS4 package with your shadPKG")
                        .clicked()
                {
                    self.run_shadpkg(ctx);
                }
                ui.separator();
                if ui
                    .add_enabled(!self.busy && !self.back_stack.is_empty(), egui::Button::new(RichText::new(ICON_BACK).size(16.0)))
                    .on_hover_text("Back to what you had open before")
                    .clicked()
                {
                    self.go_back(ctx);
                }
                if ui.add_enabled(!self.busy && self.path.is_some(), egui::Button::new(RichText::new(ICON_REFRESH).size(16.0))).clicked() {
                    if let Some(p) = self.path.clone() {
                        self.open(ctx, p, false);
                    }
                }
                if ui
                    .add(egui::SelectableLabel::new(self.show_history, RichText::new("History").size(16.0)))
                    .on_hover_text("Everything PKG Master wrote, with Revert")
                    .clicked()
                {
                    self.show_history = !self.show_history;
                }
                if self.busy {
                    ui.spinner();
                }
            });
            match &self.path {
                Some(p) => ui.label(RichText::new(p.display().to_string()).monospace()),
                None => ui.weak("No file open"),
            };
            ui.add_space(4.0);
        });

        self.history_ui(ctx);
        self.delete_ui(ctx);
        self.shadpkg_missing_ui(ctx);
        self.anyps5_missing_ui(ctx);
        self.rpcs3_missing_ui(ctx);
        self.update_ui(ctx);

        self.actions_bottom = None;
        egui::CentralPanel::default().show(ctx, |ui| {

            // The passcode row only appears for an encrypted package.
            if self.loaded.as_ref().is_some_and(|l| l.key_problem.is_some()) {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label("Passcode:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.passcode)
                            .password(true)
                            .hint_text("32 characters, often all zeros")
                            .desired_width(300.0),
                    );
                    let can_unlock = !self.busy && self.path.is_some();
                    if ui.add_enabled(can_unlock, egui::Button::new("Unlock")).clicked() {
                        if let Some(p) = self.path.clone() {
                            self.start_load(ctx, p);
                        }
                    }
                });
            }
            ui.separator();

            if let Some(rows) = self.browse.clone() {
                if rows.is_empty() {
                    ui.weak("This folder is empty.");
                    return;
                }
                let row_h = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
                let (mut clicked, mut action) = (None, None);
                egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_h, rows.len(), |ui, range| {
                    for r in &rows[range] {
                        ui.horizontal(|ui| {
                            let size = if r.is_dir { "folder".to_string() } else { fmt_size(r.size) };
                            ui.label(RichText::new(format!("{size:>10}")).monospace().weak());
                            let name = if r.is_dir { format!("{}/", r.name) } else { r.name.clone() };
                            let can_open = r.is_dir || r.openable;
                            let text = if can_open { RichText::new(name).monospace() } else { RichText::new(name).monospace().weak() };
                            let resp = ui.add_enabled(!self.busy, egui::SelectableLabel::new(false, text));
                            if resp.clicked() && can_open {
                                clicked = Some(r.path.clone());
                            }
                            resp.context_menu(|ui| {
                                if r.openable && !is_pkg_path(&r.path) && ui.button("Convert to ELF…").clicked() {
                                    action = Some(RowAction::ConvertFile(r.path.clone()));
                                    ui.close_menu();
                                }
                                file_menu(ui, &r.path, &mut action);
                            });
                        });
                    }
                });
                if let Some(p) = clicked {
                    self.start_load(ctx, p);
                } else if let Some(a) = action {
                    self.apply_row_action(ctx, a);
                }
                return;
            }

            let Some(loaded) = &self.loaded else {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    ui.heading("Drag a PKG (PS4) file here");
                    ui.weak("or open a folder containing a decrypted PS5 or PS3 game (Open folder…).");
                    if !self.library.entries.is_empty() {
                        ui.add_space(12.0);
                        ui.weak(format!("Your saved games are under {ICON_LIBRARY} Library."));
                    }
                });
                return;
            };

            // Icon, title (with the Library star), title ID and version.
            let mut do_star = false;
            let starred = self.path.as_deref().is_some_and(|p| self.library.contains(p));
            ui.horizontal(|ui| {
                if let Some(tex) = &self.icon_tex {
                    ui.add(egui::Image::new(tex).fit_to_exact_size(egui::vec2(88.0, 88.0)));
                }
                ui.vertical(|ui| {
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let fallback = self.path.as_deref().map(file_label).unwrap_or_default();
                        let title = loaded.info.title.clone().unwrap_or(fallback);
                        ui.label(RichText::new(title).size(22.0).strong());
                        let star = RichText::new(if starred { ICON_STARRED } else { ICON_STAR })
                            .size(22.0)
                            .color(if starred { Color32::from_rgb(0xE8, 0xB3, 0x2A) } else { ui.visuals().weak_text_color() });
                        do_star = ui
                            .add(egui::Button::new(star).frame(false))
                            .on_hover_text(if starred { "Remove from your Library" } else { "Save to your Library" })
                            .clicked();
                    });
                    let mut sub = Vec::new();
                    if let Some(id) = &loaded.info.title_id {
                        sub.push(id.clone());
                    }
                    if let Some(v) = &loaded.info.version {
                        sub.push(format!("version {v}"));
                    }
                    if !sub.is_empty() {
                        ui.weak(sub.join("  ·  "));
                    }
                });
            });
            ui.add_space(6.0);

            egui::Grid::new("info").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
                for (k, v) in &loaded.rows {
                    ui.strong(k);
                    ui.label(v);
                    ui.end_row();
                }
            });

            ui.add_space(8.0);
            let ready = loaded.input.is_some() && !self.busy;
            let is_package = matches!(loaded.input, Some(Input::Package(_)));
            let has_files = matches!(loaded.input, Some(Input::Package(_) | Input::Folder(_)));
            let ps3 = loaded.ps3_eboot.is_some();
            let mut do_convert = false;
            let mut menu_action = None;
            let mut do_extract = false;
            let mut do_run = false;
            let mut do_pick_anyps5 = false;
            let mut do_launch = false;
            let mut do_stop = false;
            let mut do_rpcs3 = false;
            let mut do_pick_rpcs3 = false;
            let mut do_update_anyps5 = false;
            let is_folder = matches!(loaded.input, Some(Input::Folder(_)));
            let running = self.game_running();
            // PS3 executables can't be converted; RPCS3 runs them as they are.
            if !ps3 && loaded.input.is_some() {
                ui.horizontal(|ui| {
                    let label = match (&self.selected, has_files) {
                        (Some(p), true) => format!("Convert {} to ELF…", p.rsplit('/').next().unwrap_or(p)),
                        _ => "Convert to ELF…".to_string(),
                    };
                    do_convert = ui
                        .add_enabled(ready, egui::Button::new(RichText::new(label).size(15.0)))
                        .on_hover_text("ELF is the file type AnyPS5 needs.")
                        .clicked();
                    // Only an unlocked package has files to extract.
                    if is_package {
                        do_extract = ui
                            .add_enabled(ready, egui::Button::new(RichText::new("Extract all files…").size(15.0)))
                            .on_hover_text("Unpacks everything inside the .pkg into a normal folder.")
                            .clicked();
                    }
                });
            }
            if ps3 {
                ui.horizontal(|ui| {
                    do_rpcs3 = ui
                        .add_enabled(ready && !running, egui::Button::new(RichText::new("Run in RPCS3").size(15.0)))
                        .on_hover_text("Plays this game in your RPCS3 and saves it to your Library.")
                        .clicked();
                    if running {
                        do_stop = ui.button(RichText::new("Stop").size(15.0)).clicked();
                    }
                    match find_rpcs3(&self.settings) {
                        Some(exe) => {
                            do_pick_rpcs3 = ui
                                .add_enabled(!self.busy, egui::Link::new(RichText::new(format!("RPCS3: {}", exe.display())).monospace()))
                                .on_hover_text("Click to change.")
                                .clicked();
                        }
                        None => do_pick_rpcs3 = ui.add_enabled(!self.busy, egui::Button::new("RPCS3…")).clicked(),
                    }
                });
            } else if !self.is_ps4 && loaded.input.is_some() {
                // AnyPS5 only runs PS5 games, so PS4 games get no AnyPS5 buttons.
                ui.horizontal(|ui| {
                    do_run = ui
                        .add_enabled(
                            ready && is_folder && !running,
                            egui::Button::new(RichText::new("Run in AnyPS5…").size(15.0)),
                        )
                        .on_hover_text(
                            "Converts the game's executables, runs your AnyPS5 relinker on them and lays out \
                             app.exe, libs/ and app0/ inside your AnyPS5 folder.",
                        )
                        .on_disabled_hover_text("Needs a decrypted game folder (Open folder…).")
                        .clicked();
                    let can_launch = !self.busy && !running && self.run_dir.is_some();
                    do_launch = ui.add_enabled(can_launch, egui::Button::new(RichText::new("Launch").size(15.0))).clicked();
                    if running {
                        do_stop = ui.button(RichText::new("Stop").size(15.0)).clicked();
                    }
                    match (self.settings.release(), &self.settings.anyps5_dir) {
                        // Once set, the button is replaced by the path; click it to change.
                        (Ok(r), Some(dir)) => {
                            do_pick_anyps5 = ui
                                .add_enabled(
                                    !self.busy,
                                    egui::Link::new(RichText::new(format!("AnyPS5: {}", dir.display())).monospace()),
                                )
                                .on_hover_text(format!(
                                    "Click to change.\nrelinker: {}\nlibs: {}",
                                    r.relinker.display(),
                                    r.libs.display()
                                ))
                                .clicked();
                        }
                        (Err(_), Some(_)) => {
                            do_pick_anyps5 = ui.add_enabled(!self.busy, egui::Button::new("AnyPS5 folder…")).clicked();
                            ui.weak("AnyPS5 folder incomplete, pick it again");
                        }
                        _ => {
                            do_pick_anyps5 = ui.add_enabled(!self.busy, egui::Button::new("AnyPS5 folder…")).clicked();
                        }
                    }
                });
                if let Some(tag) = &self.anyps5_update {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("AnyPS5 {tag} is out.")).color(Color32::from_rgb(0x2E, 0xA0, 0x43)));
                        do_update_anyps5 = ui
                            .add_enabled(!self.busy, egui::Button::new("Update AnyPS5").small())
                            .on_hover_text("Downloads it from AnyPS5's GitHub releases into a new folder next to your current one.")
                            .clicked();
                    });
                }
            }
            // The log stops above this line, so these buttons stay usable.
            self.actions_bottom = Some(ui.cursor().top());

            if has_files && !loaded.files.is_empty() {
                ui.add_space(8.0);
                ui.label(if ps3 {
                    "Files in the folder:"
                } else if is_package {
                    "Files in the package (click one to choose what gets converted):"
                } else {
                    "Files in the folder (click one to choose what gets converted):"
                });
                let files: Vec<_> = loaded.files.iter().filter(|f| !f.is_dir).collect();
                let folder_root = match &loaded.input {
                    Some(Input::Folder(r)) => Some(r.clone()),
                    _ => None,
                };
                let row_h = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
                let mut clicked = None;
                egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_h, files.len(), |ui, range| {
                    for f in &files[range] {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("{:>10}", fmt_size(f.size))).monospace().weak());
                            let selected = self.selected.as_deref() == Some(f.path.as_str());
                            let resp = ui.selectable_label(selected, RichText::new(&f.path).monospace());
                            if resp.clicked() {
                                clicked = Some(f.path.clone());
                            }
                            resp.context_menu(|ui| {
                                if !ps3 && ui.add_enabled(!self.busy, egui::Button::new("Convert to ELF…")).clicked() {
                                    menu_action = Some(RowAction::ConvertInner(f.path.clone()));
                                    ui.close_menu();
                                }
                                match &folder_root {
                                    // A real file on disk: all the options.
                                    Some(root) => file_menu(ui, &root.join(f.path.split('/').collect::<PathBuf>()), &mut menu_action),
                                    // Inside a .pkg: read-only, so no Delete or Explorer.
                                    None => {
                                        if ui.button("Copy path").clicked() {
                                            ui.ctx().copy_text(f.path.clone());
                                            ui.close_menu();
                                        }
                                    }
                                }
                            });
                        });
                    }
                });
                if let Some(p) = clicked {
                    self.selected = Some(p);
                }
            }

            if let Some(a) = menu_action {
                self.apply_row_action(ctx, a);
            }
            if do_star {
                self.toggle_library();
            }
            if do_convert {
                self.convert(ctx);
            }
            if do_extract {
                self.extract(ctx);
            }
            if do_pick_anyps5 {
                // Not set up yet: explain where to get it first.
                if self.settings.anyps5_dir.is_none() {
                    self.anyps5_missing = true;
                } else {
                    self.pick_anyps5();
                }
            }
            if do_update_anyps5 {
                self.update_anyps5(ctx);
            }
            if do_run {
                self.run_anyps5();
            }
            if do_launch {
                self.launch(ctx);
            }
            if do_stop {
                self.stop_game();
            }
            if do_pick_rpcs3 {
                self.rpcs3_missing = true;
            }
            if do_rpcs3 {
                if let Some(p) = self.path.clone() {
                    self.run_rpcs3(ctx, &p);
                }
            }
        });
    }
}

/// Toolbar and title icons (all in egui's built-in fonts).
const ICON_BACK: &str = "⬅";
const ICON_REFRESH: &str = "⟳";
const ICON_STAR: &str = "☆";
const ICON_STARRED: &str = "★";
const ICON_LIBRARY: &str = "📚";
