//! All dialogs (modal stack). Every `dlg_*` takes the popped modal state, returns "keep open?".
use crate::app::*;
use crate::core::*;
use crate::picker::{PickMode, PickTarget, Picker};
use eframe::egui;
use std::path::{Path, PathBuf};
use std::time::Duration;

// ---------------------------------------------------------------------------------------------
// state types
// ---------------------------------------------------------------------------------------------
pub enum OnDone {
    Extract { dest: PathBuf },
    Test,
    Create { open: PathBuf, password: String },
    Hash,
    Fetch { file: PathBuf },
}

pub struct ProgressDlg {
    pub title: String,
    pub verb: &'static str,
    pub job: Job,
    pub on_done: Option<OnDone>,
    pub paused: bool,
    pub background: bool,
    pub note: String,
    pub resized: bool,
}
impl ProgressDlg {
    pub fn new(title: String, verb: &'static str, job: Job, on_done: OnDone) -> Self {
        ProgressDlg { title, verb, job, on_done: Some(on_done), paused: false, background: false, note: String::new(), resized: false }
    }
}

pub struct ExtractDlg {
    pub archive: PathBuf,
    pub password: String,
    pub dest: String,
    pub nsel: usize,
    pub only_sel: bool,
    pub mode: String,
    pub keep: bool,
}

pub struct ExtractParams {
    pub archive: PathBuf,
    pub dest: PathBuf,
    pub password: String,
    pub keep_paths: bool,
    pub mode: String,
    pub items: Vec<(String, bool)>,
    pub total: u64,
    pub packed: u64,
    pub files: u64,
}

pub struct CreateDlg {
    pub o: CreateOpts,
    pub items: Vec<(PathBuf, bool)>,
    sel: Option<usize>,
    pw2: String,
    show_pw: bool,
    touched: bool,
    prev_level: u32,
    pub dead: bool,
}

pub struct DropDlg {
    pub paths: Vec<PathBuf>,
}

enum DropAct {
    None,
    Close,
    Open,
    Here,
    To,
    ExtractDlg,
    AddOpen,
    AddNew,
    Quick(&'static str),
}

pub struct HashView {
    pub res: HashResult,
    pub text: String,
}

pub struct BenchDlg {
    sz: SevenZip,
    job: Option<Job>,
    threads: u32,
    dict_log: u32,
    cpu: String,
    ram: String,
    os: String,
}

#[derive(Default)]
pub struct SettingsDlg {
    msg: String,
}

pub enum CreateRes {
    Started,
    Asked,
    Failed,
}

// ---------------------------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------------------------
fn shell<R: Default>(ui: &mut egui::Ui, batch: bool, id: &str, title: &str, size: [f32; 2], esc_ok: bool, f: impl FnOnce(&mut egui::Ui) -> R) -> (R, bool) {
    if batch {
        let r = egui::CentralPanel::default().show(ui, |ui| f(ui)).inner;
        return (r, false);
    }
    // every dialog is its own native window (movable, resizable); egui falls back to a draggable
    // window inside the main one when the backend has no multi-window support
    let ctx = ui.ctx().clone();
    let vid = egui::ViewportId::from_hash_of(id);
    let mut builder = egui::ViewportBuilder::default().with_title(title).with_inner_size(size).with_min_inner_size([320.0, 120.0]);
    if let Some(r) = ctx.input(|i| i.viewport().outer_rect) {
        let c = r.center();
        builder = builder.with_position(egui::pos2((c.x - size[0] / 2.0).max(0.0), (c.y - size[1] / 2.0).max(0.0)));
    }
    let mut f = Some(f);
    let mut closed = false;
    let out = ctx.show_viewport_immediate(vid, builder, |ui, class| {
        let f = f.take()?;
        let mut res = None;
        let mut content_h = 0.0f32;
        let body = |ui: &mut egui::Ui| {
            let so = egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| f(ui));
            content_h = so.content_size.y;
            res = Some(so.inner);
        };
        let embedded = class == egui::ViewportClass::EmbeddedWindow;
        if embedded {
            body(ui);
        } else {
            egui::CentralPanel::default().show(ui, |ui| body(ui));
        }
        let ctx = ui.ctx().clone();
        closed = ui.input(|i| i.viewport().close_requested());
        if esc_ok && !closed {
            closed = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        }
        let n = ctx.data_mut(|d| {
            let c: &mut u32 = d.get_temp_mut_or_insert_with(egui::Id::new(("frames", vid)), || 0u32);
            *c += 1;
            *c
        });
        if !embedded {
            // fit the window to its content: exactly in the first frames, afterwards it may only grow
            let inner = ui.input(|i| i.viewport().inner_rect).map(|r| r.size()).unwrap_or(egui::vec2(size[0], size[1]));
            let maxh = ui.input(|i| i.viewport().monitor_size).map(|m| m.y * 0.9).unwrap_or(900.0);
            let want = (content_h + 30.0).clamp(120.0, maxh.max(200.0));
            if (n <= 3 && (want - inner.y).abs() > 4.0) || (n > 3 && want > inner.y + 4.0) {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(inner.x, want)));
            }
        }
        res
    });
    (out.unwrap_or_default(), closed)
}

fn combo<T: PartialEq + Clone>(ui: &mut egui::Ui, id: &str, cur: &mut T, opts: &[(T, String)], enabled: bool, width: f32) -> bool {
    let label = opts.iter().find(|o| o.0 == *cur).map(|o| o.1.clone()).unwrap_or_default();
    let mut changed = false;
    ui.add_enabled_ui(enabled, |ui| {
        egui::ComboBox::from_id_salt(id).selected_text(label).width(width).show_ui(ui, |ui| {
            for (v, l) in opts {
                if ui.selectable_value(cur, v.clone(), l).changed() {
                    changed = true;
                }
            }
        });
    });
    changed
}

fn buttons_right(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(6.0);
    // fixed height: a bare right-to-left layout would take all the free height and the window would keep growing
    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 28.0), egui::Layout::right_to_left(egui::Align::Center), add);
}

fn max_body_h(ui: &egui::Ui) -> f32 {
    (ui.ctx().content_rect().height() - 170.0).max(200.0)
}

// ---------------------------------------------------------------------------------------------
// create dialog logic
// ---------------------------------------------------------------------------------------------
fn is_updatable(fmt: &str) -> bool {
    ["7z", "zip", "tar", "wim"].contains(&fmt)
}

impl CreateDlg {
    pub fn new(app: &App, items: Vec<(PathBuf, bool)>) -> CreateDlg {
        let mut o = CreateOpts { fmt: "7z".into(), method: "LZMA2".into(), ..Default::default() };
        let out = if items.is_empty() {
            let dir = if app.cfg.last_dir.is_empty() || !Path::new(&app.cfg.last_dir).is_dir() { crate::picker::home_dir() } else { PathBuf::from(&app.cfg.last_dir) };
            unique_path(&dir, "archive", ".7z").to_string_lossy().to_string()
        } else {
            default_archive_name(&items, "7z")
        };
        o.out = out;
        CreateDlg { o, items, sel: None, pw2: String::new(), show_pw: false, touched: false, prev_level: 5, dead: false }
    }
    /// Dialog that adds `items` to an archive that already exists (the open one).
    pub fn for_archive(app: &App, items: Vec<(PathBuf, bool)>, archive: &Path, password: &str) -> CreateDlg {
        let mut d = CreateDlg::new(app, items);
        let ext = archive.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        if is_updatable(&ext) {
            d.o.fmt = ext;
        }
        d.o.method = methods_for(&d.o.fmt).first().map(|s| s.to_string()).unwrap_or_default();
        d.o.out = archive.to_string_lossy().to_string();
        d.touched = true;
        d.o.if_exists = "add".into();
        if !password.is_empty() {
            d.o.password = password.to_string();
            d.pw2 = password.to_string();
        }
        d.normalize(true);
        d
    }
    pub fn add_items(&mut self, v: Vec<(PathBuf, bool)>) {
        for it in v {
            if !self.items.iter().any(|x| x.0 == it.0) {
                self.items.push(it);
            }
        }
        self.autoname();
    }
    pub fn set_out_dir(&mut self, dir: &Path) {
        let name = Path::new(&self.o.out).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "archive".into());
        self.o.out = dir.join(name).to_string_lossy().to_string();
        self.touched = true;
    }
    fn autoname(&mut self) {
        if !self.touched && !self.items.is_empty() {
            self.o.out = default_archive_name(&self.items, &self.o.fmt);
        }
    }
    fn swap_ext(&mut self) {
        if !self.touched && !self.items.is_empty() {
            self.autoname();
        } else {
            let stem = strip_known_ext(&self.o.out);
            self.o.out = format!("{}{}", stem, out_ext(&self.o.fmt, self.o.sfx));
        }
    }
    fn normalize(&mut self, method_changed: bool) {
        let o = &mut self.o;
        let ms = methods_for(&o.fmt);
        if ms.is_empty() {
            o.method.clear();
        } else if !ms.contains(&o.method.as_str()) {
            o.method = ms[0].to_string();
        }
        if method_changed {
            o.dict.clear();
            o.word = 0;
        }
        if !dict_options(&o.method).contains(&o.dict.as_str()) {
            o.dict.clear();
        }
        if !word_options(&o.method).contains(&o.word) {
            o.word = 0;
        }
        if o.fmt != "7z" {
            o.solid.clear();
            o.sfx = false;
            o.enc_names = false;
        }
        if o.fmt != "7z" && o.fmt != "zip" {
            o.password.clear();
            self.pw2.clear();
        }
        let spec = fmt_spec(&o.fmt);
        if spec.map(|s| s.tar).unwrap_or(false) {
            o.sdel = false;
        }
        if !["7z", "zip", "tar"].contains(&o.fmt.as_str()) {
            o.volume.clear();
        }
        if !is_updatable(&o.fmt) {
            o.update = "add".into();
        }
        if !IS_WIN {
            o.shared = false;
        }
        // level 0 means "store": 7-Zip needs the Copy method for that
        if o.level == 0 && ms.contains(&"Copy") {
            o.method = "Copy".into();
        } else if self.prev_level == 0 && o.level != 0 && o.method == "Copy" && !ms.is_empty() {
            o.method = ms[0].to_string();
        }
        self.prev_level = o.level;
    }
}

// ---------------------------------------------------------------------------------------------
// App: modal runner
// ---------------------------------------------------------------------------------------------
impl App {
    fn absorb_pending(&mut self) {
        for m in std::mem::take(&mut self.pending) {
            self.next_uid += 1;
            self.modals.push((self.next_uid, m));
        }
    }

    pub fn run_modals(&mut self, ui: &mut egui::Ui) {
        self.absorb_pending();
        let mut i = 0;
        while i < self.modals.len() {
            let uid = self.modals[i].0;
            let mut m = std::mem::replace(&mut self.modals[i].1, Modal::Taken);
            let keep = self.show_modal(ui, uid, &mut m);
            let pos = self.modals.iter().position(|e| e.0 == uid);
            match pos {
                Some(p) if keep => {
                    self.modals[p].1 = m;
                    i = p + 1;
                }
                Some(p) => {
                    self.modals.remove(p);
                    i = p;
                }
                None => i += 1,
            }
            self.absorb_pending();
        }
    }

    /// The "Add to archive" window that sits below the dialog being shown asks to be closed.
    fn close_create_below(&mut self) {
        for e in self.modals.iter_mut().rev() {
            if let Modal::Create(d) = &mut e.1 {
                d.dead = true;
                break;
            }
        }
    }

    fn show_modal(&mut self, ui: &mut egui::Ui, uid: u64, m: &mut Modal) -> bool {
        match m {
            Modal::Taken => true,
            Modal::Msg { text } => self.dlg_msg(ui, uid, text),
            Modal::Password { text, input, path, purpose } => self.dlg_password(ui, uid, text, input, path, purpose),
            Modal::Picker(p) => {
                let batch = self.batch_mode;
                let tr = &self.tr;
                let title = p.title.clone();
                let (res, closed) = shell(ui, batch, &format!("picker-{uid}"), &title, [660.0, 520.0], true, |ui| p.show(ui, tr));
                let res = if closed { Some(crate::picker::PickResult::Cancel) } else { res };
                match res {
                    Some(r) => {
                        let cur = p.current_dir().to_string();
                        self.route_pick(p.target, r, cur);
                        false
                    }
                    None => true,
                }
            }
            Modal::Extract(d) => self.dlg_extract(ui, uid, d),
            Modal::Create(d) => self.dlg_create(ui, uid, d),
            Modal::Exists { opts, noadd, out } => self.dlg_exists(ui, uid, opts, *noadd, out),
            Modal::ConfirmDelete { opts } => self.dlg_confirm_delete(ui, uid, opts),
            Modal::Progress(d) => self.dlg_progress(ui, uid, d),
            Modal::Props => self.dlg_props(ui, uid),
            Modal::HashChoose => self.dlg_hash_choose(ui, uid),
            Modal::HashView(h) => self.dlg_hash_view(ui, uid, h),
            Modal::Bench(b) => self.dlg_bench(ui, uid, b),
            Modal::Settings(s) => self.dlg_settings(ui, uid, s),
            Modal::Drop(d) => self.dlg_drop(ui, uid, d),
        }
    }

    // ---- simple dialogs
    fn dlg_msg(&mut self, ui: &mut egui::Ui, uid: u64, text: &str) -> bool {
        let tr = &self.tr;
        let (ok, esc) = shell(ui, self.batch_mode, &format!("msg-{uid}"), "Universal 7-Zip GUI", [440.0, 150.0], true, |ui| {
            ui.add(egui::Label::new(text).wrap());
            let mut ok = false;
            buttons_right(ui, |ui| {
                ok = ui.add(egui::Button::new(tr.t("ok")).min_size(egui::vec2(80.0, 0.0))).clicked();
            });
            ok
        });
        !(ok || esc)
    }

    fn dlg_password(&mut self, ui: &mut egui::Ui, uid: u64, text: &str, input: &mut String, path: &Path, purpose: &Purpose) -> bool {
        let tr = &self.tr;
        let mut act = 0; // 1 ok, 2 cancel
        let (_, esc) = shell(ui, self.batch_mode, &format!("pw-{uid}"), &tr.t("pwTitle"), [400.0, 170.0], true, |ui| {
            ui.heading(tr.t("pwTitle"));
            ui.label(text);
            let r = ui.add(egui::TextEdit::singleline(input).password(true).desired_width(f32::INFINITY));
            r.request_focus();
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                act = 1;
            }
            buttons_right(ui, |ui| {
                if ui.button(tr.t("cancel")).clicked() {
                    act = 2;
                }
                if ui.add(egui::Button::new(tr.t("ok")).min_size(egui::vec2(80.0, 0.0))).clicked() {
                    act = 1;
                }
            });
        });
        if act == 1 {
            let (p, pw, pu) = (path.to_path_buf(), input.clone(), purpose.clone());
            self.open_archive(p, pw, pu);
            return false;
        }
        !(act == 2 || esc)
    }

    // ---- extract
    fn dlg_extract(&mut self, ui: &mut egui::Ui, uid: u64, d: &mut ExtractDlg) -> bool {
        let tr = &self.tr;
        let mut act = 0; // 1 ok, 2 cancel, 3 browse
        let modes = [("aoa", tr.t("ow1")), ("aos", tr.t("ow2")), ("aou", tr.t("ow3")), ("aot", tr.t("ow4"))];
        let (_, esc) = shell(ui, self.batch_mode, &format!("extract-{uid}"), &tr.t("exTitle"), [560.0, 330.0], true, |ui| {
            let dropped: Option<PathBuf> = ui.input(|i| i.raw.dropped_files.iter().find_map(|f| { let p = f.path().to_path_buf(); Some(if p.is_dir() { p } else { p.parent()?.to_path_buf() }) }));
            if let Some(p) = dropped {
                d.dest = p.to_string_lossy().to_string();
            }
            ui.heading(tr.t("exTitle"));
            ui.add_space(4.0);
            ui.label(tr.t("exDest"));
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut d.dest).desired_width(ui.available_width() - 100.0));
                if ui.button(tr.t("browse")).clicked() {
                    act = 3;
                }
            });
            if d.nsel > 0 {
                ui.add_space(4.0);
                ui.label(tr.t("exScope"));
                ui.radio_value(&mut d.only_sel, false, tr.t("exAll"));
                ui.radio_value(&mut d.only_sel, true, tr.tf("exSel", &[&d.nsel.to_string()]));
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(tr.t("exIfExists"));
                let mut mode = d.mode.clone();
                let opts: Vec<(String, String)> = modes.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
                combo(ui, "ex_mode", &mut mode, &opts, true, 220.0);
                d.mode = mode;
            });
            ui.checkbox(&mut d.keep, tr.t("exKeep"));
            buttons_right(ui, |ui| {
                if ui.button(tr.t("cancel")).clicked() {
                    act = 2;
                }
                if ui.add(egui::Button::new(tr.t("ok")).min_size(egui::vec2(80.0, 0.0))).clicked() {
                    act = 1;
                }
            });
        });
        match act {
            3 => {
                let p = Picker::new(self.tr.t("exPickDest"), PickMode::Dir, PickTarget::ExtractDest, &d.dest);
                self.pending.push(Modal::Picker(Box::new(p)));
                true
            }
            1 => {
                let (items, total, packed, files) = match &self.arch {
                    Some(a) if a.path == d.archive => a.sizing(d.only_sel),
                    _ => (vec![], 0, 0, 0),
                };
                self.cfg.last_dir = d.dest.clone();
                self.cfg.save();
                let p = ExtractParams { archive: d.archive.clone(), dest: PathBuf::from(d.dest.trim()), password: d.password.clone(), keep_paths: d.keep, mode: d.mode.clone(), items, total, packed, files };
                self.start_extract(p);
                false
            }
            2 => false,
            _ => !esc,
        }
    }

    // ---- create
    fn dlg_create(&mut self, ui: &mut egui::Ui, uid: u64, d: &mut CreateDlg) -> bool {
        if d.dead {
            return false;
        }
        let sys_threads = self.sys.threads;
        let tr = &self.tr;
        let mut act = 0; // 1 ok, 2 cancel, 3 add items, 4 out folder
        let mut fmt_changed = false;
        let mut method_changed = false;
        let mut sfx_changed = false;
        let mut remove: Option<usize> = None;
        let batch = self.batch_mode;
        let maxh = max_body_h(ui);
        let fmt_opts: Vec<(String, String)> = FORMATS.iter().map(|f| (f.key.to_string(), fmt_label(f.key).to_string())).collect();
        let level_opts: Vec<(u32, String)> = [0u32, 1, 3, 5, 7, 9].iter().map(|l| (*l, format!("{} - {}", l, tr.t(&format!("lvl{l}"))))).collect();
        let auto = tr.t("cAuto");
        let (_, esc) = shell(ui, batch, &format!("create-{uid}"), &tr.t("cTitle"), [740.0, 640.0], true, |ui| {
            let dropped: Vec<(PathBuf, bool)> = ui.input(|i| i.raw.dropped_files.iter().map(|f| { let p = f.path().to_path_buf(); let dir = p.is_dir(); (p, dir) }).collect());
            if !dropped.is_empty() {
                d.add_items(dropped);
            }
            ui.heading(tr.t("cTitle"));
            ui.add_space(2.0);
            egui::ScrollArea::vertical().max_height(maxh).auto_shrink([false, true]).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(tr.t("cArchive"));
                    let r = ui.add(egui::TextEdit::singleline(&mut d.o.out).desired_width(ui.available_width() - 100.0));
                    if r.changed() {
                        d.touched = true;
                    }
                    if ui.button(tr.t("cFolderBtn")).clicked() {
                        act = 4;
                    }
                });
                ui.label(tr.t("cItems"));
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    egui::ScrollArea::vertical().id_salt("items").max_height(88.0).auto_shrink([false, true]).show(ui, |ui| {
                        if d.items.is_empty() {
                            ui.weak(tr.t("cNone"));
                        }
                        for (i, (p, dir)) in d.items.iter().enumerate() {
                            ui.horizontal(|ui| {
                                let name = format!("{}{}", p.to_string_lossy(), if *dir { std::path::MAIN_SEPARATOR_STR } else { "" });
                                let on = d.sel == Some(i);
                                if ui.selectable_label(on, name).clicked() {
                                    d.sel = Some(i);
                                }
                            });
                        }
                    });
                });
                ui.horizontal(|ui| {
                    if ui.button(tr.t("cAddMore")).clicked() {
                        act = 3;
                    }
                    if ui.add_enabled(d.sel.is_some(), egui::Button::new(tr.t("cRemove"))).clicked() {
                        remove = d.sel;
                    }
                });
                ui.add_space(4.0);
                ui.horizontal_top(|ui| {
                    let spec = fmt_spec(&d.o.fmt);
                    let tarfmt = spec.map(|s| s.tar).unwrap_or(false);
                    let no_level = d.o.fmt == "tar" || d.o.fmt == "wim";
                    let ms: Vec<(String, String)> = methods_for(&d.o.fmt).iter().map(|m| (m.to_string(), m.to_string())).collect();
                    let dict_o: Vec<(String, String)> = std::iter::once((String::new(), auto.clone())).chain(dict_options(&d.o.method).iter().map(|v| (v.to_string(), dict_label(v)))).collect();
                    let word_o: Vec<(u32, String)> = std::iter::once((0u32, auto.clone())).chain(word_options(&d.o.method).iter().map(|v| (*v, v.to_string()))).collect();
                    let solid_o: Vec<(String, String)> = std::iter::once((String::new(), auto.clone()))
                        .chain(SOLID_OPTIONS.iter().map(|v| (v.to_string(), match *v { "off" => tr.t("solidOff"), "on" => tr.t("solidOn"), o => dict_label(o) })))
                        .collect();
                    let thr_o: Vec<(u32, String)> = std::iter::once((0u32, auto.clone())).chain((1..=sys_threads.max(1)).map(|n| (n, n.to_string()))).collect();
                    let mem_o: Vec<(u32, String)> = std::iter::once((0u32, auto.clone())).chain([10u32, 25, 50, 75, 100].iter().map(|n| (*n, format!("{n}%")))).collect();
                    ui.vertical(|ui| {
                        ui.set_width(330.0);
                        egui::Grid::new("g_left").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                            ui.label(tr.t("cFormat"));
                            if combo(ui, "c_fmt", &mut d.o.fmt, &fmt_opts, true, 170.0) {
                                fmt_changed = true;
                            }
                            ui.end_row();
                            ui.label(tr.t("cLevel"));
                            combo(ui, "c_level", &mut d.o.level, &level_opts, !no_level, 170.0);
                            ui.end_row();
                            ui.label(tr.t("cMethod"));
                            let mut m = d.o.method.clone();
                            if combo(ui, "c_method", &mut m, &ms, !ms.is_empty() && d.o.level != 0, 170.0) {
                                d.o.method = m;
                                method_changed = true;
                            }
                            ui.end_row();
                            ui.label(tr.t("cDict"));
                            combo(ui, "c_dict", &mut d.o.dict, &dict_o, dict_o.len() > 1, 170.0);
                            ui.end_row();
                            ui.label(tr.t("cWord"));
                            combo(ui, "c_word", &mut d.o.word, &word_o, word_o.len() > 1, 170.0);
                            ui.end_row();
                            ui.label(tr.t("cSolid"));
                            combo(ui, "c_solid", &mut d.o.solid, &solid_o, d.o.fmt == "7z", 170.0);
                            ui.end_row();
                            ui.label(tr.t("cThreads"));
                            combo(ui, "c_thr", &mut d.o.threads, &thr_o, !no_level, 170.0);
                            ui.end_row();
                            ui.label(tr.t("cMem"));
                            combo(ui, "c_mem", &mut d.o.memuse, &mem_o, !no_level, 170.0);
                            ui.end_row();
                            ui.label(tr.t("cVol"));
                            ui.add_enabled(["7z", "zip", "tar"].contains(&d.o.fmt.as_str()), egui::TextEdit::singleline(&mut d.o.volume).desired_width(170.0));
                            ui.end_row();
                            ui.label(tr.t("cParams"));
                            ui.add(egui::TextEdit::singleline(&mut d.o.params).desired_width(170.0));
                            ui.end_row();
                        });
                    });
                    ui.vertical(|ui| {
                        ui.set_width(340.0);
                        let upd_o: Vec<(String, String)> = [("add", "uAdd"), ("update", "uUpdate"), ("fresh", "uFresh"), ("sync", "uSync")].iter().map(|(k, l)| (k.to_string(), tr.t(l))).collect();
                        let pm_o: Vec<(String, String)> = [("rel", "pmRel"), ("full", "pmFull"), ("abs", "pmAbs")].iter().map(|(k, l)| (k.to_string(), tr.t(l))).collect();
                        egui::Grid::new("g_right").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                            ui.label(tr.t("cUpdate"));
                            combo(ui, "c_upd", &mut d.o.update, &upd_o, is_updatable(&d.o.fmt) && !tarfmt, 190.0);
                            ui.end_row();
                            ui.label(tr.t("cPath"));
                            combo(ui, "c_pm", &mut d.o.pathmode, &pm_o, true, 190.0);
                            ui.end_row();
                        });
                        ui.add_space(4.0);
                        ui.group(|ui| {
                            ui.set_width(ui.available_width());
                            ui.strong(tr.t("cOpts"));
                            if ui.add_enabled(d.o.fmt == "7z", egui::Checkbox::new(&mut d.o.sfx, tr.t("cSfx"))).changed() {
                                sfx_changed = true;
                            }
                            ui.add_enabled(IS_WIN, egui::Checkbox::new(&mut d.o.shared, tr.t("cShared")));
                            ui.add_enabled(!tarfmt, egui::Checkbox::new(&mut d.o.sdel, tr.t("cDel")));
                        });
                        ui.add_space(4.0);
                        ui.group(|ui| {
                            ui.set_width(ui.available_width());
                            let enc = d.o.fmt == "7z" || d.o.fmt == "zip";
                            ui.strong(tr.t("cEnc"));
                            egui::Grid::new("g_enc").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                                ui.label(tr.t("cPw"));
                                ui.add_enabled(enc, egui::TextEdit::singleline(&mut d.o.password).password(!d.show_pw).desired_width(150.0));
                                ui.end_row();
                                ui.label(tr.t("cPw2"));
                                ui.add_enabled(enc, egui::TextEdit::singleline(&mut d.pw2).password(!d.show_pw).desired_width(150.0));
                                ui.end_row();
                                ui.label("");
                                ui.add_enabled(enc, egui::Checkbox::new(&mut d.show_pw, tr.t("cShowPw")));
                                ui.end_row();
                                ui.label(tr.t("cEncMethod"));
                                let em: Vec<(String, String)> = vec![("AES256".into(), "AES-256".into()), ("ZipCrypto".into(), "ZipCrypto".into())];
                                combo(ui, "c_enc", &mut d.o.enc_method, &em, enc && d.o.fmt == "zip", 150.0);
                                ui.end_row();
                            });
                            ui.add_enabled(enc && d.o.fmt == "7z", egui::Checkbox::new(&mut d.o.enc_names, tr.t("cEncNames")));
                        });
                    });
                });
                if let Some(s) = fmt_spec(&d.o.fmt) {
                    if s.single && (d.items.len() != 1 || d.items[0].1) {
                        ui.add_space(4.0);
                        ui.colored_label(ui.visuals().warn_fg_color, tr.tf("cSingleWarn", &[&d.o.fmt]));
                    }
                }
            });
            buttons_right(ui, |ui| {
                if ui.button(tr.t("cancel")).clicked() {
                    act = 2;
                }
                if ui.add(egui::Button::new(tr.t("ok")).min_size(egui::vec2(80.0, 0.0))).clicked() {
                    act = 1;
                }
            });
        });
        if let Some(i) = remove {
            if i < d.items.len() {
                d.items.remove(i);
            }
            d.sel = None;
            d.autoname();
        }
        if fmt_changed {
            d.normalize(true);
            d.swap_ext();
        } else {
            d.normalize(method_changed);
            if sfx_changed {
                d.swap_ext();
            }
        }
        match act {
            4 => {
                let dir = Path::new(&d.o.out).parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
                let p = Picker::new(self.tr.t("cPickOut"), PickMode::Dir, PickTarget::CreateOutDir, &dir);
                self.pending.push(Modal::Picker(Box::new(p)));
                true
            }
            3 => {
                let start = if self.cfg.last_dir.is_empty() { d.items.first().and_then(|i| i.0.parent().map(|p| p.to_string_lossy().to_string())).unwrap_or_default() } else { self.cfg.last_dir.clone() };
                let p = Picker::new(self.tr.t("cPickItems"), PickMode::Multi, PickTarget::CreateItems, &start);
                self.pending.push(Modal::Picker(Box::new(p)));
                true
            }
            1 => {
                if d.items.is_empty() {
                    let m = self.t("cNoItems");
                    self.msg(m);
                    return true;
                }
                if d.o.password != d.pw2 {
                    let m = self.t("cPwMismatch");
                    self.msg(m);
                    return true;
                }
                let mut o = d.o.clone();
                o.sources = d.items.iter().map(|i| i.0.clone()).collect();
                if o.sdel {
                    self.pending.push(Modal::ConfirmDelete { opts: Box::new(o) });
                    return true;
                }
                !matches!(self.start_create(o), CreateRes::Started)
            }
            2 => false,
            _ => !esc,
        }
    }

    fn dlg_confirm_delete(&mut self, ui: &mut egui::Ui, uid: u64, opts: &mut CreateOpts) -> bool {
        let tr = &self.tr;
        let mut act = 0;
        let (_, esc) = shell(ui, self.batch_mode, &format!("confdel-{uid}"), "Universal 7-Zip GUI", [420.0, 140.0], true, |ui| {
            ui.add(egui::Label::new(tr.t("cDelConfirm")).wrap());
            buttons_right(ui, |ui| {
                if ui.button(tr.t("cancel")).clicked() {
                    act = 2;
                }
                if ui.add(egui::Button::new(tr.t("ok")).min_size(egui::vec2(80.0, 0.0))).clicked() {
                    act = 1;
                }
            });
        });
        if act == 1 {
            self.close_create_below();
            let o = opts.clone();
            self.start_create(o);
            return false;
        }
        !(act == 2 || esc)
    }

    fn dlg_exists(&mut self, ui: &mut egui::Ui, uid: u64, opts: &mut CreateOpts, noadd: bool, out: &Path) -> bool {
        let tr = &self.tr;
        let mut act = 0; // 1 overwrite 2 add 3 cancel
        let (_, esc) = shell(ui, self.batch_mode, &format!("exists-{uid}"), "Universal 7-Zip GUI", [480.0, 150.0], true, |ui| {
            ui.add(egui::Label::new(tr.tf("cExists", &[&out.to_string_lossy()])).wrap());
            buttons_right(ui, |ui| {
                if ui.button(tr.t("cancel")).clicked() {
                    act = 3;
                }
                if !noadd && ui.button(tr.t("cAddTo")).clicked() {
                    act = 2;
                }
                if ui.button(tr.t("cOverwrite")).clicked() {
                    act = 1;
                }
            });
        });
        if act == 1 || act == 2 {
            self.close_create_below();
            let mut o = opts.clone();
            o.if_exists = if act == 1 { "overwrite".into() } else { "add".into() };
            self.start_create(o);
            return false;
        }
        !(act == 3 || esc)
    }

    pub fn start_create(&mut self, opts: CreateOpts) -> CreateRes {
        let Some(sz) = self.sz.clone() else { return CreateRes::Failed };
        match prepare_create(&sz.exe, &opts) {
            Err(e) => {
                self.msg_err(&e);
                CreateRes::Failed
            }
            Ok(Prepared::Exists { noadd, out }) => {
                self.pending.push(Modal::Exists { opts: Box::new(opts), noadd, out });
                CreateRes::Asked
            }
            Ok(Prepared::Ready(plan)) => {
                let Plan { args, opts: mut jo, open, .. } = *plan;
                if let Some((b, f)) = walk_size(&opts.sources) {
                    jo.total = Some(b);
                    jo.files_total = Some(f);
                }
                let title = if self.batch_mode { self.batch_title_for(&open) } else { self.tr.t("tCreate") };
                self.start_progress(title, "vAdd", args, jo, OnDone::Create { open, password: opts.password.clone() });
                CreateRes::Started
            }
        }
    }

    // ---- dropped files: ask what to do with them
    fn dlg_drop(&mut self, ui: &mut egui::Ui, uid: u64, d: &mut DropDlg) -> bool {
        let n = d.paths.len();
        let all_arc = n > 0 && d.paths.iter().all(|p| p.is_file() && looks_like_archive(p));
        let name_of = |p: &Path| p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let summary = if n == 1 { name_of(&d.paths[0]) } else { d.paths.iter().take(4).map(|p| name_of(p)).collect::<Vec<_>>().join(", ") + if n > 4 { ", …" } else { "" } };
        let tr = &self.tr;
        let l_open = tr.t("mOpenArchive");
        let l_here = tr.t("mExtractHere");
        let l_to = if n == 1 { tr.tf("mExtractTo", &[&folder_name_for(&d.paths[0])]) } else { tr.tf("mExtractTo", &["…"]) };
        let l_dlg = tr.t("mExtractFiles");
        let l_new = tr.t("mAddArchive");
        let l_q7 = tr.tf("mAddTo", &["….7z"]);
        let l_qz = tr.tf("mAddTo", &["….zip"]);
        let open_arc = self.arch.as_ref().filter(|a| {
            let e = a.path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            is_updatable(&e)
        });
        let l_open_add = open_arc.map(|a| tr.tf("mAddTo", &[&name_of(&a.path)]));
        let mut act = DropAct::None;
        let (_, closed) = shell(ui, self.batch_mode, &format!("drop-{uid}"), &tr.t("dropTitle"), [480.0, 330.0], true, |ui| {
            ui.heading(tr.t("dropTitle"));
            ui.add(egui::Label::new(egui::RichText::new(&summary).weak()).truncate());
            ui.add_space(8.0);
            let w = ui.available_width();
            let btn = |ui: &mut egui::Ui, label: &str| ui.add_sized([w, 30.0], egui::Button::new(label)).clicked();
            if all_arc {
                if n == 1 && btn(ui, &l_open) {
                    act = DropAct::Open;
                }
                if btn(ui, &l_here) {
                    act = DropAct::Here;
                }
                if btn(ui, &l_to) {
                    act = DropAct::To;
                }
                if n == 1 && btn(ui, &l_dlg) {
                    act = DropAct::ExtractDlg;
                }
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(6.0);
            }
            if let Some(l) = &l_open_add {
                if btn(ui, l) {
                    act = DropAct::AddOpen;
                }
            }
            if btn(ui, &l_new) {
                act = DropAct::AddNew;
            }
            if btn(ui, &l_q7) {
                act = DropAct::Quick("7z");
            }
            if btn(ui, &l_qz) {
                act = DropAct::Quick("zip");
            }
            buttons_right(ui, |ui| {
                if ui.add(egui::Button::new(tr.t("cancel")).min_size(egui::vec2(80.0, 0.0))).clicked() {
                    act = DropAct::Close;
                }
            });
        });
        let items = || d.paths.iter().map(|p| (p.clone(), p.is_dir())).collect::<Vec<_>>();
        match act {
            DropAct::None => !closed,
            DropAct::Close => false,
            DropAct::Open => {
                self.open_archive(d.paths[0].clone(), String::new(), Purpose::Normal);
                false
            }
            DropAct::ExtractDlg => {
                self.open_archive(d.paths[0].clone(), String::new(), Purpose::ExtractDialog);
                false
            }
            DropAct::Here => {
                self.spawn_self(&["extract-here"], &d.paths);
                false
            }
            DropAct::To => {
                self.spawn_self(&["extract-to"], &d.paths);
                false
            }
            DropAct::Quick(f) => {
                self.spawn_self(&["quick-add", f], &d.paths);
                false
            }
            DropAct::AddNew => {
                self.push_create(items());
                false
            }
            DropAct::AddOpen => {
                if let Some(a) = &self.arch {
                    let dlg = CreateDlg::for_archive(self, items(), &a.path, &a.password);
                    self.pending.push(Modal::Create(Box::new(dlg)));
                }
                false
            }
        }
    }

    // ---- progress
    fn dlg_progress(&mut self, ui: &mut egui::Ui, uid: u64, d: &mut ProgressDlg) -> bool {
        let s = d.job.snapshot();
        self.ctx.request_repaint_after(Duration::from_millis(100));
        let rc = s.rc.unwrap_or(0);
        let paused_txt = if d.paused { format!(" ({})", self.tr.t("paused")) } else { String::new() };
        self.prog_title = Some(match s.pct {
            Some(p) if !s.done => format!("{p}%{paused_txt} · {}", d.title),
            _ => d.title.clone(),
        });
        if s.done {
            if s.cancelled {
                self.status = self.tr.t("cancelled");
                return false;
            }
            if rc == 0 {
                let od = d.on_done.take();
                self.finish_job(od, &d.job);
                return false;
            }
        }
        if s.done && self.batch_mode && !d.resized {
            d.resized = true;
            self.ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(720.0, 560.0)));
        }
        let tr = &self.tr;
        let batch = self.batch_mode;
        let wtitle = self.prog_title.clone().unwrap_or_else(|| d.title.clone());
        let mut close = false;
        let mut copy_log = false;
        let (_, closed) = shell(ui, batch, &format!("progress-{uid}"), &wtitle, [500.0, 340.0], false, |ui| {
            ui.heading(&d.title);
            ui.add_space(4.0);
            let dash = "-".to_string();
            let sz = |v: Option<f64>| v.map(|x| fmt_size(x as u64)).unwrap_or_else(|| dash.clone());
            egui::Grid::new("pgrid").num_columns(4).spacing([16.0, 6.0]).min_col_width(90.0).show(ui, |ui| {
                ui.label(tr.t("pElapsed"));
                ui.label(fmt_hms(s.el));
                ui.label(tr.t("pTotal"));
                ui.label(s.total.map(fmt_size).unwrap_or_else(|| dash.clone()));
                ui.end_row();
                ui.label(tr.t("pRemain"));
                ui.label(s.rem.map(fmt_hms).unwrap_or_else(|| dash.clone()));
                ui.label(tr.t("pSpeed"));
                ui.label(s.speed.map(|v| format!("{}/s", fmt_mb(v))).unwrap_or_else(|| dash.clone()));
                ui.end_row();
                ui.label(tr.t("pFiles"));
                ui.label(match (s.files, s.files_total) {
                    (Some(a), Some(b)) => format!("{a} / {b}"),
                    (Some(a), None) => a.to_string(),
                    _ => dash.clone(),
                });
                ui.label(tr.t("pProc"));
                ui.label(sz(s.processed));
                ui.end_row();
                ui.label(tr.t("pRatio"));
                ui.label(s.ratio.map(|v| format!("{v:.0}%")).unwrap_or_else(|| dash.clone()));
                ui.label(tr.t("pComp"));
                ui.label(sz(s.comp));
                ui.end_row();
            });
            ui.add_space(6.0);
            if !s.cur.is_empty() && !s.done {
                ui.add(egui::Label::new(egui::RichText::new(format!("{}: {}", tr.t(d.verb), s.cur)).weak()).truncate());
            }
            let bar = match s.pct {
                Some(p) => egui::ProgressBar::new(p as f32 / 100.0).show_percentage(),
                None => egui::ProgressBar::new(0.0).animate(true),
            };
            ui.scope(|ui| {
                let dark = ui.visuals().dark_mode;
                ui.visuals_mut().extreme_bg_color = if dark { egui::Color32::from_gray(44) } else { egui::Color32::from_gray(214) };
                ui.add(bar.desired_width(ui.available_width()).corner_radius(3.0));
            });
            if s.done {
                ui.add_space(6.0);
                ui.colored_label(ui.visuals().error_fg_color, tr.t(if rc == 1 { "doneWarn" } else { "doneErr" }));
                ui.label(if rc == 1 { tr.t("noteWarn") } else { tr.tf("noteErr", &[&rc.to_string()]) });
                let mut text = d.job.text();
                egui::ScrollArea::vertical().max_height(220.0).stick_to_bottom(true).show(ui, |ui| {
                    ui.add(egui::TextEdit::multiline(&mut text).font(egui::TextStyle::Monospace).desired_rows(8).desired_width(f32::INFINITY));
                });
                buttons_right(ui, |ui| {
                    if ui.add(egui::Button::new(tr.t("close")).min_size(egui::vec2(80.0, 0.0))).clicked() {
                        close = true;
                    }
                    if ui.button(tr.t("pCopyLog")).clicked() {
                        copy_log = true;
                    }
                });
            } else {
                if !d.note.is_empty() {
                    ui.colored_label(ui.visuals().warn_fg_color, &d.note);
                }
                buttons_right(ui, |ui| {
                    if ui.add(egui::Button::new(tr.t("cancel")).min_size(egui::vec2(80.0, 0.0))).clicked() {
                        d.job.cancel();
                    }
                    if ui.add(egui::Button::new(tr.t(if d.paused { "bResume" } else { "bPause" })).min_size(egui::vec2(80.0, 0.0))).clicked() {
                        let on = !d.paused;
                        if d.job.set_paused(on) {
                            d.paused = on;
                        }
                    }
                    if ui.add(egui::Button::new(tr.t(if d.background { "bFg" } else { "bBg" })).min_size(egui::vec2(80.0, 0.0))).clicked() {
                        let on = !d.background;
                        if d.job.set_background(on) {
                            d.background = on;
                            d.note.clear();
                        } else {
                            d.note = tr.t("prioFail");
                        }
                    }
                });
            }
        });
        if closed {
            if s.done {
                close = true;
            } else {
                d.job.cancel();
            }
        }
        if copy_log {
            self.ctx.copy_text(d.job.text());
        }
        if close {
            let od = d.on_done.take();
            if rc == 1 && matches!(od, Some(OnDone::Extract { .. }) | Some(OnDone::Create { .. })) {
                self.finish_job(od, &d.job);
            } else {
                self.status = self.tr.t("doneErr");
            }
            return false;
        }
        true
    }

    fn finish_job(&mut self, od: Option<OnDone>, job: &Job) {
        match od {
            Some(OnDone::Extract { dest }) => {
                self.status = self.tf("exStatus", &[&dest.to_string_lossy()]);
            }
            Some(OnDone::Test) => {
                let m = self.t("testOk");
                self.status = m.clone();
                self.msg(m);
            }
            Some(OnDone::Create { open, password }) => {
                self.status = self.tf("cCreated", &[&open.to_string_lossy()]);
                if !self.batch_mode {
                    self.open_archive(open, password, Purpose::Normal);
                }
            }
            Some(OnDone::Hash) => {
                let text = job.text();
                let res = parse_hash(&text);
                if res.rows.is_empty() && res.items.is_empty() {
                    let m = self.t("hNoResult");
                    self.msg(m);
                } else {
                    self.pending.push(Modal::HashView(Box::new(HashView { res, text })));
                }
            }
            Some(OnDone::Fetch { file }) => open_with_default(&file),
            None => {}
        }
    }

    // ---- properties
    fn dlg_props(&mut self, ui: &mut egui::Ui, uid: u64) -> bool {
        let tr = &self.tr;
        let Some(a) = &self.arch else { return false };
        let (close, esc) = shell(ui, self.batch_mode, &format!("props-{uid}"), &tr.t("propTitle"), [560.0, 460.0], true, |ui| {
            ui.heading(tr.t("propTitle"));
            ui.add_space(4.0);
            egui::ScrollArea::vertical().max_height(max_body_h(ui)).show(ui, |ui| {
                egui::Grid::new("props").num_columns(2).spacing([16.0, 6.0]).striped(true).show(ui, |ui| {
                    for (k, v) in &a.data.props {
                        ui.label(tr.prop(k));
                        ui.add(egui::Label::new(v).wrap());
                        ui.end_row();
                    }
                    ui.label(tr.t("pSize"));
                    ui.label(format!("{} ({})", fmt_size(a.size), group_digits(a.size)));
                    ui.end_row();
                    ui.label(tr.t("pPacked"));
                    ui.label(format!("{} ({})", fmt_size(a.packed), group_digits(a.packed)));
                    ui.end_row();
                    ui.label(tr.t("pFolders"));
                    ui.label(group_digits(a.folders));
                    ui.end_row();
                    ui.label(tr.t("pFilesN"));
                    ui.label(group_digits(a.files));
                    ui.end_row();
                });
            });
            let mut close = false;
            buttons_right(ui, |ui| {
                close = ui.add(egui::Button::new(tr.t("close")).min_size(egui::vec2(80.0, 0.0))).clicked();
            });
            close
        });
        !(close || esc)
    }

    // ---- checksums
    fn dlg_hash_choose(&mut self, ui: &mut egui::Ui, uid: u64) -> bool {
        let tr = &self.tr;
        let nsel = self.arch.as_ref().map(|a| a.selected.len()).unwrap_or(0);
        let has_arch = self.arch.is_some();
        let mut act = 0; // 1 selected 2 whole 3 disk 4 cancel
        let (_, esc) = shell(ui, self.batch_mode, &format!("hashc-{uid}"), &tr.t("hTitle"), [520.0, 250.0], true, |ui| {
            ui.heading(tr.t("hTitle"));
            ui.add(egui::Label::new(tr.t("hChoose")).wrap());
            ui.add_space(6.0);
            let w = ui.available_width();
            if nsel > 0 && ui.add(egui::Button::new(tr.tf("hSrcSel", &[&nsel.to_string()])).min_size(egui::vec2(w, 30.0))).clicked() {
                act = 1;
            }
            if has_arch && ui.add(egui::Button::new(tr.t("hSrcAll")).min_size(egui::vec2(w, 30.0))).clicked() {
                act = 2;
            }
            if ui.add(egui::Button::new(tr.t("hSrcDisk")).min_size(egui::vec2(w, 30.0))).clicked() {
                act = 3;
            }
            buttons_right(ui, |ui| {
                if ui.button(tr.t("cancel")).clicked() {
                    act = 4;
                }
            });
        });
        match act {
            1 | 2 => {
                if let Some(a) = &self.arch {
                    let (items, total, packed, files) = a.sizing(act == 1);
                    let args = hash_archive_args(&a.path, &a.password, &items);
                    let title = self.tr.t("tHash");
                    self.start_progress(title, "vHash", args, JobOpts { total: Some(total), packed: Some(packed), files_total: Some(files), ..Default::default() }, OnDone::Hash);
                }
                false
            }
            3 => {
                let p = Picker::new(self.tr.t("hPick"), PickMode::Multi, PickTarget::HashDisk, &self.cfg.last_dir);
                self.pending.push(Modal::Picker(Box::new(p)));
                false
            }
            4 => false,
            _ => !esc,
        }
    }

    fn dlg_hash_view(&mut self, ui: &mut egui::Ui, uid: u64, h: &mut HashView) -> bool {
        let tr = &self.tr;
        let mut copy: Option<String> = None;
        let maxh = max_body_h(ui);
        let (close, esc) = shell(ui, self.batch_mode, &format!("hashv-{uid}"), &tr.t("hTitle"), [840.0, 520.0], true, |ui| {
            ui.heading(tr.t("hTitle"));
            ui.add_space(4.0);
            egui::ScrollArea::both().max_height(maxh).auto_shrink([false, true]).show(ui, |ui| {
                if let Some(sz) = h.res.size {
                    ui.label(format!("{}: {}", tr.t("pSize"), tr.tf("hBytes", &[&group_digits(sz), &group_digits(sz / 1024)])));
                }
                if let Some(f) = h.res.files {
                    ui.label(format!("{}: {}", tr.t("pFilesN"), group_digits(f)));
                }
                ui.add_space(4.0);
                egui::Grid::new("hrows").num_columns(4).spacing([14.0, 5.0]).striped(true).show(ui, |ui| {
                    for r in &h.res.rows {
                        for (kind, val) in [("hData", &r.data), ("hDataNames", &r.names)] {
                            let Some(v) = val else { continue };
                            ui.label(&r.name);
                            ui.weak(tr.tf(kind, &[""]).trim().to_string());
                            ui.label(egui::RichText::new(v).monospace());
                            if ui.small_button(tr.t("copy")).clicked() {
                                copy = Some(v.clone());
                            }
                            ui.end_row();
                        }
                    }
                });
                if !h.res.items.is_empty() {
                    ui.add_space(6.0);
                    egui::CollapsingHeader::new(tr.tf("hPerFile", &[&h.res.items.len().to_string()])).show(ui, |ui| {
                        for it in &h.res.items {
                            ui.strong(&it.name);
                            for (k, v) in &it.hashes {
                                ui.label(egui::RichText::new(format!("{k}: {v}")).monospace());
                            }
                            ui.add_space(4.0);
                        }
                    });
                }
            });
            let mut close = false;
            buttons_right(ui, |ui| {
                close = ui.add(egui::Button::new(tr.t("close")).min_size(egui::vec2(80.0, 0.0))).clicked();
                if ui.button(tr.t("copy")).clicked() {
                    copy = Some(h.text.clone());
                }
            });
            close
        });
        if let Some(c) = copy {
            self.ctx.copy_text(c);
        }
        !(close || esc)
    }

    // ---- benchmark
    fn dlg_bench(&mut self, ui: &mut egui::Ui, uid: u64, b: &mut BenchDlg) -> bool {
        let tr = &self.tr;
        let running = b.job.as_ref().map(|j| !j.is_done()).unwrap_or(false);
        if running {
            self.ctx.request_repaint_after(Duration::from_millis(200));
        }
        let lines = b.job.as_ref().map(|j| j.lines()).unwrap_or_default();
        let data = parse_bench(&lines);
        let (el, rc) = b.job.as_ref().map(|j| { let s = j.snapshot(); (s.el, s.rc) }).unwrap_or((0.0, None));
        let thr_max = self.sys.threads.max(1);
        let mut act = 0; // 1 start 2 stop 3 close
        let maxh = max_body_h(ui);
        let dict_o: Vec<(u32, String)> = (18u32..=30).map(|l| (l, fmt_mb((1u64 << l) as f64))).collect();
        let thr_o: Vec<(u32, String)> = (1..=thr_max).map(|n| (n, n.to_string())).collect();
        let raw = b.job.as_ref().map(|j| j.text()).unwrap_or_default();
        let (_, esc) = shell(ui, self.batch_mode, &format!("bench-{uid}"), &tr.t("bnTitle"), [820.0, 620.0], true, |ui| {
            ui.heading(tr.t("bnTitle"));
            egui::ScrollArea::vertical().max_height(maxh).auto_shrink([false, true]).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(tr.t("bnDict"));
                    combo(ui, "b_dict", &mut b.dict_log, &dict_o, !running, 100.0);
                    ui.label(tr.t("bnThreads"));
                    combo(ui, "b_thr", &mut b.threads, &thr_o, !running, 70.0);
                    if running {
                        if ui.button(tr.t("bnStop")).clicked() {
                            act = 2;
                        }
                    } else if ui.button(tr.t(if b.job.is_some() { "bnRestart" } else { "bnStart" })).clicked() {
                        act = 1;
                    }
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(format!("{}: {}", tr.t("bnElapsed"), fmt_hms(el)));
                    if let Some(ru) = data.ram_use {
                        ui.label(format!("{}: {} MB", tr.t("bnMem"), ru));
                    }
                });
                ui.add_space(4.0);
                for (title, which) in [(tr.t("bnComp"), 0), (tr.t("bnDec"), 1)] {
                    ui.group(|ui| {
                        ui.set_width(ui.available_width());
                        ui.strong(title);
                        egui::Grid::new(("bg", which)).num_columns(5).spacing([18.0, 4.0]).striped(true).show(ui, |ui| {
                            for h in [tr.t("bnCDict"), tr.t("bnCSpeed"), tr.t("bnCUsage"), tr.t("bnCRU"), tr.t("bnCRating")] {
                                ui.strong(h);
                            }
                            ui.end_row();
                            for r in &data.rows {
                                let v = if which == 0 { r.c } else { r.d };
                                ui.label(fmt_mb((1u64 << r.dict.min(40)) as f64));
                                ui.label(format!("{} KB/s", group_digits(v[0])));
                                ui.label(format!("{}%", v[1]));
                                ui.label(group_digits(v[2]));
                                ui.label(group_digits(v[3]));
                                ui.end_row();
                            }
                            if let Some((c, d)) = &data.avr {
                                let v = if which == 0 { c } else { d };
                                ui.strong(tr.t("bnRes"));
                                ui.strong(format!("{} KB/s", group_digits(v[0])));
                                ui.strong(format!("{}%", v[1]));
                                ui.strong(group_digits(v[2]));
                                ui.strong(group_digits(v[3]));
                                ui.end_row();
                            }
                        });
                    });
                }
                if let Some(t) = data.tot {
                    ui.add_space(4.0);
                    ui.strong(format!("{}: {}", tr.t("bnTotal"), group_digits(t[2])));
                }
                if let (false, Some(c), true) = (running, rc, data.rows.is_empty()) {
                    if c != 0 && c != 255 {
                        ui.colored_label(ui.visuals().error_fg_color, tr.tf("bnErr", &[&c.to_string()]));
                    }
                }
                ui.add_space(4.0);
                let ram = if b.ram.is_empty() { String::new() } else { format!("  |  RAM: {}", b.ram) };
                ui.weak(format!("{}  |  {}{}  |  {} threads", b.cpu, b.os, ram, thr_max));
                egui::CollapsingHeader::new(tr.t("bnRaw")).default_open(std::env::var("U7Z_RAW").is_ok()).show(ui, |ui| {
                    ui.add(egui::Label::new(egui::RichText::new(raw.clone()).monospace()).wrap());
                });
            });
            buttons_right(ui, |ui| {
                if ui.add(egui::Button::new(tr.t("close")).min_size(egui::vec2(80.0, 0.0))).clicked() {
                    act = 3;
                }
            });
        });
        match act {
            1 => {
                if let Some(j) = &b.job {
                    j.cancel();
                }
                b.job = Some(Job::start(&b.sz.exe, bench_args(b.threads, b.dict_log, 10), JobOpts { use_pty: !IS_WIN, ..Default::default() }));
                true
            }
            2 => {
                if let Some(j) = &b.job {
                    j.cancel();
                }
                true
            }
            3 => {
                if let Some(j) = &b.job {
                    j.cancel();
                }
                false
            }
            _ => {
                if esc {
                    if let Some(j) = &b.job {
                        j.cancel();
                    }
                }
                !esc
            }
        }
    }

    // ---- settings
    fn dlg_settings(&mut self, ui: &mut egui::Ui, uid: u64, s: &mut SettingsDlg) -> bool {
        let tr = &self.tr;
        let mut new_theme: Option<String> = None;
        let mut new_lang: Option<String> = None;
        let mut shell_act = 0;
        let cur_theme = self.cfg.theme.clone();
        let cur_lang = tr.lang.clone();
        let langs = tr.langs.clone();
        let banner = self.sz.as_ref().map(|z| format!("{}\n{}", z.exe.to_string_lossy(), z.banner)).unwrap_or_default();
        let themes: Vec<(String, String)> = [("auto", "themeAuto"), ("light", "themeLight"), ("dark", "themeDark"), ("oled", "themeOled")].iter().map(|(k, l)| (k.to_string(), tr.t(l))).collect();
        let mut close = false;
        let (_, esc) = shell(ui, self.batch_mode, &format!("settings-{uid}"), &tr.t("settings"), [540.0, 470.0], true, |ui| {
            ui.heading(tr.t("settings"));
            ui.add_space(4.0);
            egui::Grid::new("sgrid").num_columns(2).spacing([14.0, 8.0]).show(ui, |ui| {
                ui.label(tr.t("theme"));
                let mut th = cur_theme.clone();
                if combo(ui, "s_theme", &mut th, &themes, true, 200.0) {
                    new_theme = Some(th);
                }
                ui.end_row();
                ui.label(tr.t("lang"));
                let lo: Vec<(String, String)> = langs.iter().map(|(c, n)| (c.clone(), n.clone())).collect();
                let mut l = cur_lang.clone();
                if combo(ui, "s_lang", &mut l, &lo, true, 200.0) {
                    new_lang = Some(l);
                }
                ui.end_row();
            });
            ui.add_space(8.0);
            ui.separator();
            ui.strong(tr.t("shellTitle"));
            ui.horizontal(|ui| {
                if ui.button(tr.t("shellInstall")).clicked() {
                    shell_act = 1;
                }
                if ui.button(tr.t("shellRemove")).clicked() {
                    shell_act = 2;
                }
            });
            if !s.msg.is_empty() {
                ui.label(&s.msg);
            }
            ui.weak(tr.t("shellHint"));
            if !banner.is_empty() {
                ui.add_space(6.0);
                ui.weak(&banner);
            }
            ui.add_space(8.0);
            ui.separator();
            ui.strong(tr.t("about"));
            ui.weak(format!("Universal 7-Zip GUI (u7zgui) {} : Copyright (c) 2026 Oktay Mercan", env!("CARGO_PKG_VERSION")));
            let url = "https://github.com/OKTAYMERCAN/Universal-7zip-GUI";
            if ui.add(egui::Link::new(url)).on_hover_text(url).clicked() {
                open_url(url);
            }
            buttons_right(ui, |ui| {
                close = ui.add(egui::Button::new(tr.t("close")).min_size(egui::vec2(80.0, 0.0))).clicked();
            });
        });
        if let Some(t) = new_theme {
            self.set_theme(&t);
        }
        if let Some(l) = new_lang {
            self.set_lang(&l);
        }
        if shell_act != 0 {
            let r = if shell_act == 1 { crate::shell::install() } else { crate::shell::uninstall() };
            s.msg = match r {
                Ok(()) => self.t("shellDone"),
                Err(e) => self.tf("shellFail", &[&e]),
            };
        }
        !(close || esc)
    }
}

impl BenchDlg {
    pub fn new(sz: SevenZip, sys: &SysInfo) -> BenchDlg {
        let threads = sys.threads.max(1);
        let dict_log: u32 = std::env::var("U7Z_BENCH_DICT").ok().and_then(|v| v.parse().ok()).unwrap_or(24);
        let job = Some(Job::start(&sz.exe, bench_args(threads, dict_log, 10), JobOpts { use_pty: !IS_WIN, ..Default::default() }));
        BenchDlg { sz, job, threads, dict_log, cpu: sys.cpu.clone(), ram: sys.ram_mb.map(|m| format!("{m} MB")).unwrap_or_default(), os: format!("{} {}", sys.os, sys.arch) }
    }
}
