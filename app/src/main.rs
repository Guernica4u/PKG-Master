//! PKG Master: a small window for opening a PS4/PS5 package, converting its
//! executable to ELF, or extracting all of its files.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

use eframe::egui::{self, Color32, RichText};
use pkgmaster::binary::fmt_size;
use pkgmaster::convert::{convert_to_elf, default_output_name, extract_all, load, Input, Loaded};
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
}

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
            (Input::Package(_), Some(p), _) => p.clone(),
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
                                    Some(Input::Package(_)) => Status::Ok(format!(
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

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(!self.busy, egui::Button::new(RichText::new("Open file…").size(16.0))).clicked() {
                    self.pick_file(ctx);
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
                    ui.weak("Fake-signed and debug PS4/PS5 packages, or a SELF/ELF file.");
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
            let mut do_convert = false;
            let mut do_extract = false;
            ui.horizontal(|ui| {
                let label = match (&self.selected, is_package) {
                    (Some(p), true) => format!("Convert {} to ELF…", p.rsplit('/').next().unwrap_or(p)),
                    _ => "Convert to ELF…".to_string(),
                };
                do_convert = ui.add_enabled(ready, egui::Button::new(RichText::new(label).size(15.0))).clicked();
                do_extract = ui
                    .add_enabled(ready && is_package, egui::Button::new(RichText::new("Extract all files…").size(15.0)))
                    .clicked();
            });

            if is_package && !loaded.files.is_empty() {
                ui.add_space(8.0);
                ui.label("Files in the package (click one to choose what gets converted):");
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
        });
    }
}
