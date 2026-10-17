//! PKG Master: a small window for opening a PS4/PS5 package, converting its
//! executable to ELF, extracting all of its files, or exporting a package or
//! decrypted game folder for AnyPS5, and running a game folder through the
//! user's own AnyPS5 relinker.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use eframe::egui::{self, Color32, RichText};
use pkgmaster::anyps5::{
    find_libs, find_relinker, launch_command, preflight, run_relinker, settings_path, spawn_piped, stream_lines, Settings, APP_EXE,
};
use pkgmaster::binary::fmt_size;
use pkgmaster::convert::{
    convert_to_elf, default_output_name, export_anyps5, extract_all, load, Input, Loaded, MODULE_DIR,
};
use pkgmaster::Error;

const PACKAGE_EXTS: [&str; 1] = ["pkg"];
const BINARY_EXTS: [&str; 5] = ["bin", "self", "sprx", "prx", "elf"];

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("PKG Master")
            .with_inner_size([760.0, 560.0])
            .with_min_inner_size([520.0, 400.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    // A file passed on the command line (or dropped onto the .exe) opens at start.
    let initial = std::env::args_os().nth(1).map(PathBuf::from);
    eframe::run_native(
        "PKG Master",
        options,
        Box::new(move |cc| {
            let mut app = App::new();
            if let Some(path) = initial {
                app.start_load(&cc.egui_ctx, path);
            }
            Ok(Box::new(app))
        }),
    )
}

enum Msg {
    Loaded(PathBuf, Result<Loaded, Error>),
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
        }
    }

    fn passcode(&self) -> Option<String> {
        let p = self.passcode.trim();
        (!p.is_empty()).then(|| p.to_string())
    }

    fn start_load(&mut self, ctx: &egui::Context, path: PathBuf) {
        self.busy = true;
        self.loaded = None;
        self.selected = None;
        self.progress = None;
        self.status = Status::Info(format!("Opening {}…", file_label(&path)));
        self.path = Some(path.clone());
        let (tx, ctx, passcode) = (self.tx.clone(), ctx.clone(), self.passcode());
        thread::spawn(move || {
            let result = load(&path, passcode.as_deref());
            let _ = tx.send(Msg::Loaded(path, result));
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
        let inner = self.selected.clone();
        let source_name = match (&input, &inner, &self.path) {
            (Input::Package(_) | Input::Folder(_), Some(p), _) => p.clone(),
            (_, _, Some(path)) => path.to_string_lossy().into_owned(),
            _ => "eboot.bin".into(),
        };
        let mut dialog = rfd::FileDialog::new()
            .set_title("Save ELF as")
            .set_file_name(default_output_name(&source_name))
            .add_filter("ELF file", &["elf"]);
        if let Some(dir) = self.path.as_ref().and_then(|p| p.parent()) {
            dialog = dialog.set_directory(dir);
        }
        let Some(out) = dialog.save_file() else { return };
        self.status = Status::Info("Converting…".into());
        self.spawn_job(ctx, move |progress| {
            let r = convert_to_elf(&input, inner.as_deref(), &out, progress)?;
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
        self.spawn_job(ctx, move |progress| {
            let n = extract_all(&pfs, &out, progress)?;
            Ok(format!("Extracted {n} file(s) to {}.", out.display()))
        });
    }

    fn export(&mut self, ctx: &egui::Context) {
        let Some(input) = self.loaded.as_ref().and_then(|l| l.input.clone()) else { return };
        if matches!(input, Input::Binary(_)) {
            return;
        }
        let mut dialog = rfd::FileDialog::new().set_title("Export for AnyPS5 into");
        if let Some(dir) = self.path.as_ref().and_then(|p| p.parent()) {
            dialog = dialog.set_directory(dir);
        }
        let Some(dir) = dialog.pick_folder() else { return };
        let name = self
            .path
            .as_ref()
            .and_then(|p| if p.is_dir() { p.file_name() } else { p.file_stem() })
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "game".into());
        let out = dir.join(format!("{name}-anyps5"));
        self.status = Status::Info("Exporting for AnyPS5…".into());
        self.spawn_job(ctx, move |progress| {
            let r = export_anyps5(&input, &out, progress)?;
            let mut msg = format!(
                "Exported to {}: eboot.elf, {} module(s), {} original file(s). See HOW-TO-RUN.txt.",
                out.display(),
                r.modules.len(),
                r.app0_files
            );
            for (name, why) in &r.skipped {
                msg.push_str(&format!("\nSkipped {MODULE_DIR}/{name}: {why}"));
            }
            for w in &r.warnings {
                msg.push_str("\nNote: ");
                msg.push_str(w);
            }
            Ok(msg)
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
        if self.settings.release().is_err() && !self.pick_anyps5() {
            return;
        }
        let pre = match preflight(&root) {
            Ok(p) => p,
            Err(e) => {
                self.status = Status::Err(describe_error(&e));
                return;
            }
        };
        let mut dialog = rfd::FileDialog::new().set_title("Lay out the AnyPS5 run folder inside");
        if let Some(dir) = root.parent() {
            dialog = dialog.set_directory(dir);
        }
        let Some(dir) = dialog.pick_folder() else { return };
        let name = root.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "game".into());
        self.confirm = Some((pre.warnings(), root, dir.join(format!("{name}-anyps5-run"))));
    }

    fn start_run(&mut self, ctx: &egui::Context, root: PathBuf, out: PathBuf) {
        let release = match self.settings.release() {
            Ok(r) => r,
            Err(e) => {
                self.status = Status::Err(describe_error(&e));
                return;
            }
        };
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
            let result = run_relinker(&root, &release, &out, &progress, &log).map(|r| r.out_dir);
            let _ = tx.send(Msg::Relinked(result));
            ctx.request_repaint();
        });
    }

    fn launch(&mut self, ctx: &egui::Context) {
        let Some(dir) = self.run_dir.clone() else { return };
        let mut child = match launch_command(&dir).and_then(spawn_piped) {
            Ok(c) => c,
            Err(e) => {
                self.status = Status::Err(describe_error(&e));
                return;
            }
        };
        self.log.push(format!("> {APP_EXE} (in {})", dir.display()));
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
        self.status = Status::Info(format!("{APP_EXE} is running."));
        let (game, tx, ctx) = (self.game.clone(), self.tx.clone(), ctx.clone());
        thread::spawn(move || loop {
            thread::sleep(Duration::from_millis(200));
            let mut guard = game.lock().unwrap();
            let Some(child) = guard.as_mut() else { break };
            let msg = match child.try_wait() {
                Ok(None) => continue,
                Ok(Some(status)) => format!("{APP_EXE} exited ({status})."),
                Err(e) => format!("Lost track of {APP_EXE}: {e}"),
            };
            *guard = None;
            let _ = tx.send(Msg::Exited(msg));
            ctx.request_repaint();
            break;
        });
    }

    fn stop_game(&mut self) {
        if let Some(child) = self.game.lock().unwrap().as_mut() {
            let _ = child.kill();
        }
    }

    fn game_running(&self) -> bool {
        self.game.lock().map(|g| g.is_some()).unwrap_or(false)
    }

    fn handle_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Loaded(path, result) => {
                    self.busy = false;
                    if self.path.as_deref() != Some(path.as_path()) {
                        continue;
                    }
                    match result {
                        Ok(l) => {
                            self.status = match &l.key_problem {
                                Some(Error::KeyRequired(_)) => {
                                    Status::Err("This package is encrypted. Enter its 32-character passcode and press Unlock.".into())
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
                            let msg = format!("Ready in {}. Press Launch to start {APP_EXE}.", dir.display());
                            self.run_dir = Some(dir);
                            Status::Ok(msg)
                        }
                        Err(e) => Status::Err(describe_error(&e)),
                    };
                }
                Msg::Exited(m) => {
                    self.log.push(m.clone());
                    self.status = Status::Info(m);
                }
                Msg::Done(result) => {
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

fn file_label(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
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
        self.handle_messages();

        let dropped = ctx.input(|i| i.raw.dropped_files.first().and_then(|f| f.path.clone()));
        if let Some(path) = dropped {
            if !self.busy {
                self.start_load(ctx, path);
            }
        }

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.add_space(6.0);
            if let Some((done, total, item)) = &self.progress {
                let frac = if *total > 0 { *done as f32 / *total as f32 } else { 0.0 };
                ui.add(egui::ProgressBar::new(frac).show_percentage().text(item.as_str()));
            }
            let (text, color) = match &self.status {
                Status::Info(m) => (m.as_str(), ui.visuals().text_color()),
                Status::Ok(m) => (m.as_str(), Color32::from_rgb(0x2E, 0xA0, 0x43)),
                Status::Err(m) => (m.as_str(), Color32::from_rgb(0xD0, 0x3B, 0x3B)),
            };
            ui.label(RichText::new(text).color(color));
            ui.add_space(6.0);
        });

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
            egui::TopBottomPanel::bottom("log").resizable(true).default_height(160.0).show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.strong("AnyPS5 log");
                    if ui.small_button("Clear").clicked() {
                        self.log.clear();
                    }
                });
                egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
                    for line in &self.log {
                        ui.label(RichText::new(line).monospace().size(12.0));
                    }
                });
            });
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(!self.busy, egui::Button::new(RichText::new("Open file…").size(16.0))).clicked() {
                    self.pick_file(ctx);
                }
                if ui.add_enabled(!self.busy, egui::Button::new(RichText::new("Open folder…").size(16.0))).clicked() {
                    self.pick_folder(ctx);
                }
                match &self.path {
                    Some(p) => ui.label(RichText::new(p.display().to_string()).monospace()),
                    None => ui.weak("No file open"),
                };
                if self.busy {
                    ui.spinner();
                }
            });

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("Passcode:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.passcode)
                        .password(true)
                        .hint_text("only for encrypted packages")
                        .desired_width(300.0),
                );
                let can_unlock = !self.busy && self.path.is_some();
                if ui.add_enabled(can_unlock, egui::Button::new("Unlock")).clicked() {
                    if let Some(p) = self.path.clone() {
                        self.start_load(ctx, p);
                    }
                }
            });
            ui.separator();

            let Some(loaded) = &self.loaded else {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    ui.heading("Drop a .pkg file here");
                    ui.weak("Fake-signed and debug PS4/PS5 packages, a SELF/ELF file,");
                    ui.weak("or a decrypted game folder you dumped yourself (Open folder…).");
                    ui.weak("Retail packages need console keys and can't be opened.");
                });
                return;
            };

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
            let mut do_convert = false;
            let mut do_extract = false;
            let mut do_export = false;
            let mut do_run = false;
            let mut do_pick_anyps5 = false;
            let mut do_launch = false;
            let mut do_stop = false;
            let is_folder = matches!(loaded.input, Some(Input::Folder(_)));
            ui.horizontal(|ui| {
                let label = match (&self.selected, has_files) {
                    (Some(p), true) => format!("Convert {} to ELF…", p.rsplit('/').next().unwrap_or(p)),
                    _ => "Convert to ELF…".to_string(),
                };
                do_convert = ui.add_enabled(ready, egui::Button::new(RichText::new(label).size(15.0))).clicked();
                do_extract = ui
                    .add_enabled(ready && is_package, egui::Button::new(RichText::new("Extract all files…").size(15.0)))
                    .clicked();
                do_export = ui
                    .add_enabled(ready && has_files, egui::Button::new(RichText::new("Export for AnyPS5…").size(15.0)))
                    .on_hover_text(
                        "Writes source/eboot.elf, source/sce_module/ (modules as ELF), app0/ (original files) \
                         and HOW-TO-RUN.txt into a folder you pick.",
                    )
                    .clicked();
            });
            ui.horizontal(|ui| {
                let running = self.game_running();
                do_run = ui
                    .add_enabled(
                        ready && is_folder && !running,
                        egui::Button::new(RichText::new("Run in AnyPS5…").size(15.0)),
                    )
                    .on_hover_text(
                        "Converts the game's executables, runs your AnyPS5 relinker on them and lays out \
                         app.exe, libs/ and app0/ in a folder you pick.",
                    )
                    .on_disabled_hover_text("Needs a decrypted game folder (Open folder…).")
                    .clicked();
                let can_launch = !self.busy && !running && self.run_dir.is_some();
                do_launch = ui.add_enabled(can_launch, egui::Button::new(RichText::new("Launch").size(15.0))).clicked();
                if running {
                    do_stop = ui.button(RichText::new("Stop").size(15.0)).clicked();
                }
                do_pick_anyps5 = ui.add_enabled(!self.busy, egui::Button::new("AnyPS5 folder…")).clicked();
                match self.settings.release() {
                    Ok(r) => ui
                        .label(RichText::new(r.libs.parent().unwrap_or(&r.libs).display().to_string()).monospace().weak())
                        .on_hover_text(format!("relinker: {}
libs: {}", r.relinker.display(), r.libs.display())),
                    Err(_) if self.settings.anyps5_dir.is_some() => ui.weak("AnyPS5 folder incomplete, pick it again"),
                    Err(_) => ui.weak("AnyPS5 folder not set"),
                };
            });

            if has_files && !loaded.files.is_empty() {
                ui.add_space(8.0);
                ui.label(if is_package {
                    "Files in the package (click one to choose what gets converted):"
                } else {
                    "Files in the folder (click one to choose what gets converted):"
                });
                let files: Vec<_> = loaded.files.iter().filter(|f| !f.is_dir).collect();
                let row_h = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
                let mut clicked = None;
                egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_h, files.len(), |ui, range| {
                    for f in &files[range] {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("{:>10}", fmt_size(f.size))).monospace().weak());
                            let selected = self.selected.as_deref() == Some(f.path.as_str());
                            if ui.selectable_label(selected, RichText::new(&f.path).monospace()).clicked() {
                                clicked = Some(f.path.clone());
                            }
                        });
                    }
                });
                if let Some(p) = clicked {
                    self.selected = Some(p);
                }
            }

            if do_convert {
                self.convert(ctx);
            }
            if do_extract {
                self.extract(ctx);
            }
            if do_export {
                self.export(ctx);
            }
            if do_pick_anyps5 {
                self.pick_anyps5();
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
        });
    }
}
