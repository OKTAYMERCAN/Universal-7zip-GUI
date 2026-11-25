//! In-app file/folder picker (no GTK/portal dependency, identical on every OS).
use crate::i18n::I18n;
use eframe::egui;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PickMode {
    File,
    Dir,
    Multi,
}
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PickTarget {
    OpenArchive,
    ExtractDest,
    CreateItems,
    CreateOutDir,
    HashDisk,
}
pub enum PickResult {
    Cancel,
    Path(PathBuf),
    Multi(Vec<(PathBuf, bool)>),
}

struct PEntry {
    name: String,
    full: PathBuf,
    dir: bool,
    size: Option<u64>,
}

pub struct Picker {
    pub title: String,
    pub mode: PickMode,
    pub target: PickTarget,
    cur: String,
    addr: String,
    entries: Vec<PEntry>,
    parent: Option<String>,
    hidden: bool,
    selected: Vec<(PathBuf, bool)>,
    sel_file: Option<PathBuf>,
    error: Option<String>,
    newname: Option<String>,
    to_load: Option<String>,
}

#[cfg(windows)]
fn drives() -> Vec<PathBuf> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetLogicalDrives() -> u32;
    }
    let mask = unsafe { GetLogicalDrives() };
    (0..26).filter(|i| mask & (1 << i) != 0).map(|i| PathBuf::from(format!("{}:\\", (b'A' + i as u8) as char))).collect()
}

/// Small painted folder / file icon (the bundled fonts have no emoji glyphs).
pub fn paint_icon(ui: &mut egui::Ui, dir: bool) {
    let (ir, _) = ui.allocate_exact_size(egui::vec2(16.0, 14.0), egui::Sense::hover());
    if dir {
        let col = egui::Color32::from_rgb(232, 184, 74);
        ui.painter().rect_filled(egui::Rect::from_min_size(ir.min + egui::vec2(0.0, 1.0), egui::vec2(7.0, 4.0)), 1.0, col);
        ui.painter().rect_filled(egui::Rect::from_min_max(ir.min + egui::vec2(0.0, 3.0), ir.max), 2.0, col);
    } else {
        let pr = egui::Rect::from_min_max(ir.min + egui::vec2(2.0, 0.0), ir.max - egui::vec2(2.0, 0.0));
        ui.painter().rect(pr, 1.5, ui.visuals().extreme_bg_color, egui::Stroke::new(1.0, ui.visuals().weak_text_color()), egui::StrokeKind::Inside);
    }
}

pub fn home_dir() -> PathBuf {
    let k = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(k).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

impl Picker {
    pub fn new(title: String, mode: PickMode, target: PickTarget, start: &str) -> Picker {
        let mut s = start.to_string();
        if s.is_empty() || !Path::new(&s).exists() {
            s = "~".into();
        }
        Picker {
            title,
            mode,
            target,
            cur: String::new(),
            addr: String::new(),
            entries: vec![],
            parent: None,
            hidden: false,
            selected: vec![],
            sel_file: None,
            error: None,
            newname: None,
            to_load: Some(s),
        }
    }

    pub fn current_dir(&self) -> &str {
        &self.cur
    }

    fn load(&mut self, raw: &str) {
        self.error = None;
        self.sel_file = None;
        #[cfg(windows)]
        if raw == "@drives" {
            self.cur = String::new();
            self.addr = String::new();
            self.parent = None;
            self.entries = drives().into_iter().map(|d| PEntry { name: d.to_string_lossy().to_string(), full: d, dir: true, size: None }).collect();
            return;
        }
        let mut path = if raw == "~" || raw.is_empty() { home_dir() } else { PathBuf::from(raw) };
        if path.is_file() {
            path = path.parent().map(|p| p.to_path_buf()).unwrap_or(path);
        }
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        #[cfg(windows)]
        let path = PathBuf::from(path.to_string_lossy().trim_start_matches(r"\\?\").to_string());
        let rd = match std::fs::read_dir(&path) {
            Ok(r) => r,
            Err(e) => {
                self.error = Some(e.to_string());
                return;
            }
        };
        let mut v = Vec::new();
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            #[allow(unused_mut)]
            let mut hid = name.starts_with('.');
            let md = e.metadata();
            #[cfg(windows)]
            if let Ok(m) = &md {
                use std::os::windows::fs::MetadataExt;
                if m.file_attributes() & 2 != 0 {
                    hid = true;
                }
            }
            if hid && !self.hidden {
                continue;
            }
            let (dir, size) = match &md {
                Ok(m) => {
                    let d = if m.file_type().is_symlink() { e.path().is_dir() } else { m.is_dir() };
                    (d, if d { None } else { Some(m.len()) })
                }
                Err(_) => (false, None),
            };
            v.push(PEntry { name, full: e.path(), dir, size });
        }
        v.sort_by(|a, b| (!a.dir, a.name.to_lowercase()).cmp(&(!b.dir, b.name.to_lowercase())));
        self.entries = v;
        self.parent = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => Some(p.to_string_lossy().to_string()),
            _ => {
                if cfg!(windows) { Some("@drives".to_string()) } else { None }
            }
        };
        self.cur = path.to_string_lossy().to_string();
        self.addr = self.cur.clone();
    }

    pub fn show(&mut self, ui: &mut egui::Ui, tr: &I18n) -> Option<PickResult> {
        if let Some(p) = self.to_load.take() {
            self.load(&p);
        }
        let mut result: Option<PickResult> = None;
        let mut goto: Option<String> = None;
        {
            ui.heading(&self.title);
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(self.parent.is_some(), egui::Button::new("..")).on_hover_text(tr.t("pickUp")).clicked() {
                    goto = self.parent.clone();
                }
                if ui.button("~").on_hover_text(tr.t("pickHome")).clicked() {
                    goto = Some("~".into());
                }
                if cfg!(windows) && ui.button(tr.t("drives")).on_hover_text(tr.t("drives")).clicked() {
                    goto = Some("@drives".into());
                }
                let r = ui.add(egui::TextEdit::singleline(&mut self.addr).desired_width(f32::INFINITY));
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    goto = Some(self.addr.clone());
                }
            });
            let mut hid = self.hidden;
            if ui.checkbox(&mut hid, tr.t("showHidden")).changed() {
                self.hidden = hid;
                goto = Some(self.cur.clone());
            }
            if let Some(e) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            let mut rows: Vec<usize> = Vec::new();
            let _ = &mut rows;
            let n = self.entries.len() + usize::from(self.parent.is_some());
            let has_parent = self.parent.is_some();
            let mut toggle: Option<usize> = None;
            let mut pick_file: Option<usize> = None;
            let mut dbl_file: Option<usize> = None;
            egui::ScrollArea::vertical().max_height(320.0).min_scrolled_height(320.0).auto_shrink([false, false]).show_rows(ui, 22.0, n.max(1), |ui, range| {
                if n == 0 {
                    ui.weak(tr.t("emptyFolder"));
                    return;
                }
                for i in range {
                    if has_parent && i == 0 {
                        let mut clicked = false;
                        ui.horizontal(|ui| {
                            paint_icon(ui, true);
                            clicked = ui.selectable_label(false, "..").clicked();
                        });
                        if clicked {
                            goto = self.parent.clone();
                        }
                        continue;
                    }
                    let idx = i - usize::from(has_parent);
                    let e = &self.entries[idx];
                    ui.horizontal(|ui| {
                        if self.mode == PickMode::Multi {
                            let mut on = self.selected.iter().any(|(p, _)| *p == e.full);
                            if ui.checkbox(&mut on, "").changed() {
                                toggle = Some(idx);
                            }
                        }
                        let on = self.sel_file.as_ref() == Some(&e.full);
                        paint_icon(ui, e.dir);
                        let r = ui.selectable_label(on, &e.name);
                        if r.clicked() {
                            if e.dir {
                                goto = Some(e.full.to_string_lossy().to_string());
                            } else if self.mode == PickMode::Multi {
                                toggle = Some(idx);
                            } else if self.mode == PickMode::File {
                                pick_file = Some(idx);
                            }
                        }
                        if r.double_clicked() && !e.dir && self.mode == PickMode::File {
                            dbl_file = Some(idx);
                        }
                        if let Some(s) = e.size {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.weak(crate::core::fmt_size(s));
                            });
                        }
                    });
                }
            });
            if let Some(i) = toggle {
                let e = &self.entries[i];
                if let Some(pos) = self.selected.iter().position(|(p, _)| *p == e.full) {
                    self.selected.remove(pos);
                } else {
                    self.selected.push((e.full.clone(), e.dir));
                }
            }
            if let Some(i) = pick_file {
                self.sel_file = Some(self.entries[i].full.clone());
            }
            if let Some(i) = dbl_file {
                result = Some(PickResult::Path(self.entries[i].full.clone()));
            }
            ui.add_space(6.0);
            if self.newname.is_some() {
                let mut name = self.newname.clone().unwrap_or_default();
                let mut create = false;
                ui.horizontal(|ui| {
                    ui.label(tr.t("newFolderTitle"));
                    ui.add(egui::TextEdit::singleline(&mut name).desired_width(200.0));
                    if ui.button(tr.t("create")).clicked() && !name.trim().is_empty() {
                        create = true;
                    }
                });
                if create {
                    let p = Path::new(&self.cur).join(name.trim());
                    match std::fs::create_dir(&p) {
                        Ok(_) => goto = Some(p.to_string_lossy().to_string()),
                        Err(e) => self.error = Some(e.to_string()),
                    }
                    self.newname = None;
                } else {
                    self.newname = Some(name);
                }
            }
            ui.horizontal(|ui| {
                if self.mode == PickMode::Dir && ui.button(tr.t("newFolder")).clicked() {
                    self.newname = Some(String::new());
                }
                if self.mode == PickMode::Multi && !self.selected.is_empty() {
                    ui.weak(tr.tf("selCount", &[&self.selected.len().to_string()]));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    match self.mode {
                        PickMode::Dir => {
                            if ui.add_enabled(!self.cur.is_empty(), egui::Button::new(tr.t("selectFolder"))).clicked() {
                                result = Some(PickResult::Path(PathBuf::from(&self.cur)));
                            }
                        }
                        PickMode::File => {
                            if ui.add_enabled(self.sel_file.is_some(), egui::Button::new(tr.t("btnOpen"))).clicked() {
                                result = self.sel_file.clone().map(PickResult::Path);
                            }
                        }
                        PickMode::Multi => {
                            if ui.add_enabled(!self.selected.is_empty(), egui::Button::new(tr.t("btnAdd"))).clicked() {
                                result = Some(PickResult::Multi(self.selected.clone()));
                            }
                        }
                    }
                    if ui.button(tr.t("cancel")).clicked() {
                        result = Some(PickResult::Cancel);
                    }
                });
            });
        }
        if let Some(g) = goto {
            self.load(&g);
        }
        result
    }
}
