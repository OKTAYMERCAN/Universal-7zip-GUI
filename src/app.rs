//! Application state, main window, archive tree and command handling.
//! Dialogs live in dialogs.rs.
use crate::core::*;
use crate::dialogs::*;
use crate::i18n::{self, I18n};
use crate::picker::{PickMode, PickResult, PickTarget, Picker};
use crate::settings::Settings;
use eframe::egui;
use egui_extras::{Column, TableBuilder};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

// ---------------------------------------------------------------------------------------------
// archive tree
// ---------------------------------------------------------------------------------------------
pub struct Node {
    pub key: String,
    pub name: String,
    pub dir: bool,
    pub orig: String,
    pub size: u64,
    pub packed: Option<u64>,
    pub modified: String,
    pub attr: String,
    pub enc: bool,
    pub parent: usize,
    pub children: Vec<usize>,
    pub total: u64,
    pub nfiles: u64,
}

pub struct Tree {
    pub nodes: Vec<Node>,
}

fn new_dir(key: &str, name: &str, parent: usize) -> Node {
    Node { key: key.into(), name: name.into(), dir: true, orig: key.into(), size: 0, packed: None, modified: String::new(), attr: String::new(), enc: false, parent, children: vec![], total: 0, nfiles: 0 }
}

fn ensure_dir(nodes: &mut Vec<Node>, idx: &mut HashMap<String, usize>, key: &str) -> usize {
    if let Some(i) = idx.get(key) {
        return *i;
    }
    let (pk, name) = match key.rfind('/') {
        Some(p) => (&key[..p], &key[p + 1..]),
        None => ("", key),
    };
    let parent = ensure_dir(nodes, idx, pk);
    nodes.push(new_dir(key, name, parent));
    let i = nodes.len() - 1;
    nodes[parent].children.push(i);
    idx.insert(key.to_string(), i);
    i
}

impl Tree {
    pub fn build(entries: &[Entry]) -> Tree {
        let mut nodes = vec![new_dir("", "", 0)];
        let mut idx: HashMap<String, usize> = HashMap::new();
        idx.insert(String::new(), 0);
        for e in entries {
            let key = e.key.trim_start_matches("./").trim_start_matches('/').trim_end_matches('/').to_string();
            if key.is_empty() {
                continue;
            }
            if e.dir {
                let i = ensure_dir(&mut nodes, &mut idx, &key);
                nodes[i].orig = e.path.trim_end_matches(['/', '\\']).to_string();
                nodes[i].modified = e.modified.clone();
                nodes[i].attr = e.attr.clone();
                continue;
            }
            let (pk, name) = match key.rfind('/') {
                Some(p) => (key[..p].to_string(), key[p + 1..].to_string()),
                None => (String::new(), key.clone()),
            };
            let parent = ensure_dir(&mut nodes, &mut idx, &pk);
            nodes.push(Node {
                key: key.clone(),
                name,
                dir: false,
                orig: e.path.clone(),
                size: e.size.unwrap_or(0),
                packed: e.packed,
                modified: e.modified.clone(),
                attr: e.attr.clone(),
                enc: e.enc,
                parent,
                children: vec![],
                total: 0,
                nfiles: 0,
            });
            let i = nodes.len() - 1;
            nodes[parent].children.push(i);
            idx.insert(key, i);
        }
        let mut t = Tree { nodes };
        t.finish(0);
        t
    }
    fn finish(&mut self, i: usize) {
        let mut kids = self.nodes[i].children.clone();
        kids.sort_by(|&a, &b| {
            let (x, y) = (&self.nodes[a], &self.nodes[b]);
            (!x.dir, x.name.to_lowercase()).cmp(&(!y.dir, y.name.to_lowercase()))
        });
        let (mut sum, mut cnt) = (0u64, 0u64);
        for &c in &kids {
            if self.nodes[c].dir {
                self.finish(c);
                sum += self.nodes[c].total;
                cnt += self.nodes[c].nfiles;
            } else {
                sum += self.nodes[c].size;
                cnt += 1;
            }
        }
        let n = &mut self.nodes[i];
        n.children = kids;
        n.total = sum;
        n.nfiles = cnt;
        if n.dir && i != 0 {
            n.size = sum;
        }
    }
}

pub struct Opened {
    pub path: PathBuf,
    pub password: String,
    pub data: ArchiveData,
    pub tree: Tree,
    pub expanded: HashSet<usize>,
    pub selected: HashSet<usize>,
    pub anchor: Option<usize>,
    pub flat: Vec<(usize, u32)>,
    pub dirty: bool,
    pub size: u64,
    pub packed: u64,
    pub files: u64,
    pub folders: u64,
}

impl Opened {
    pub fn rebuild_flat(&mut self) {
        let mut out = Vec::new();
        fn walk(t: &Tree, exp: &HashSet<usize>, i: usize, depth: u32, out: &mut Vec<(usize, u32)>) {
            for &c in &t.nodes[i].children {
                out.push((c, depth));
                if t.nodes[c].dir && exp.contains(&c) {
                    walk(t, exp, c, depth + 1, out);
                }
            }
        }
        walk(&self.tree, &self.expanded, 0, 0, &mut out);
        self.flat = out;
        self.dirty = false;
    }
    pub fn top_selected(&self) -> Vec<usize> {
        let mut v: Vec<usize> = self
            .selected
            .iter()
            .copied()
            .filter(|&n| {
                let mut p = self.tree.nodes[n].parent;
                while p != 0 {
                    if self.selected.contains(&p) {
                        return false;
                    }
                    p = self.tree.nodes[p].parent;
                }
                true
            })
            .collect();
        v.sort();
        v
    }
    /// (items for 7z, total bytes, packed estimate, file count) of the selection or whole archive.
    pub fn sizing(&self, only_selected: bool) -> (Vec<(String, bool)>, u64, u64, u64) {
        if !only_selected || self.selected.is_empty() {
            return (vec![], self.size, self.packed, self.files);
        }
        let top = self.top_selected();
        let items: Vec<(String, bool)> = top.iter().map(|&i| (self.tree.nodes[i].orig.clone(), self.tree.nodes[i].dir)).collect();
        let total: u64 = top.iter().map(|&i| if self.tree.nodes[i].dir { self.tree.nodes[i].total } else { self.tree.nodes[i].size }).sum();
        let files: u64 = top.iter().map(|&i| if self.tree.nodes[i].dir { self.tree.nodes[i].nfiles } else { 1 }).sum();
        let packed = if self.size > 0 { (self.packed as f64 * total as f64 / self.size as f64).round() as u64 } else { 0 };
        (items, total, packed, files)
    }
}

// ---------------------------------------------------------------------------------------------
// modal stack
// ---------------------------------------------------------------------------------------------
pub enum Modal {
    Msg { text: String },
    Password { text: String, input: String, path: PathBuf, purpose: Purpose },
    Picker(Box<Picker>),
    Extract(Box<ExtractDlg>),
    Create(Box<CreateDlg>),
    Exists { opts: Box<CreateOpts>, noadd: bool, out: PathBuf },
    ConfirmDelete { opts: Box<CreateOpts> },
    Progress(Box<ProgressDlg>),
    Props,
    HashChoose,
    HashView(Box<HashView>),
    Bench(Box<BenchDlg>),
    Settings(Box<SettingsDlg>),
}

#[derive(Clone)]
pub enum Purpose {
    Normal,
    ExtractDialog,
    BatchExtract { dest: PathBuf },
    BatchTest,
}

pub enum Task {
    ExtractHere(PathBuf),
    ExtractTo(PathBuf),
    Test(PathBuf),
    QuickAdd { fmt: String, sources: Vec<PathBuf> },
}

#[derive(Default)]
pub struct Startup {
    pub open: Option<PathBuf>,
    pub add: Vec<PathBuf>,
    pub extract_dialog: Option<PathBuf>,
    pub hash: Vec<PathBuf>,
    pub batch: Vec<Task>,
    pub shot: Option<(PathBuf, u64)>,
    pub script: String,
}

struct OpenTask {
    rx: mpsc::Receiver<Result<ArchiveData, OpenError>>,
    path: PathBuf,
    password: String,
    purpose: Purpose,
}

pub struct App {
    pub ctx: egui::Context,
    pub cfg: Settings,
    pub tr: I18n,
    pub sz: Option<SevenZip>,
    pub sys: SysInfo,
    pub arch: Option<Opened>,
    open_task: Option<OpenTask>,
    pub modals: Vec<Modal>,
    pub pending: Vec<Modal>,
    pub status: String,
    batch: Vec<Task>,
    pub batch_mode: bool,
    pub batch_total: usize,
    pub batch_done: usize,
    shot: Option<(PathBuf, u64)>,
    script: Vec<String>,
    script_next: u64,
    last_title: String,
    pub prog_title: Option<String>,
    pub tmp_dirs: Vec<PathBuf>,
    tmp_counter: u32,
    font_lang: String,
}

// ---------------------------------------------------------------------------------------------
// fonts and themes
// ---------------------------------------------------------------------------------------------
fn cjk_font() -> Option<(Vec<u8>, u32)> {
    let mut cands: Vec<(String, u32)> = Vec::new();
    #[cfg(target_os = "linux")]
    {
        if let Ok(o) = std::process::Command::new("fc-match").args(["-f", "%{file}\n%{index}", ":lang=zh-cn"]).output() {
            let t = String::from_utf8_lossy(&o.stdout).to_string();
            let mut it = t.lines();
            if let (Some(f), Some(i)) = (it.next(), it.next()) {
                cands.push((f.to_string(), i.trim().parse().unwrap_or(0)));
            }
        }
        for p in [
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
            "/usr/share/fonts/wenquanyi/wqy-microhei/wqy-microhei.ttc",
            "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
        ] {
            cands.push((p.to_string(), 0));
        }
    }
    if cfg!(windows) {
        for p in ["C:\\Windows\\Fonts\\msyh.ttc", "C:\\Windows\\Fonts\\simhei.ttf", "C:\\Windows\\Fonts\\simsun.ttc"] {
            cands.push((p.to_string(), 0));
        }
    }
    if cfg!(target_os = "macos") {
        for p in ["/System/Library/Fonts/PingFang.ttc", "/System/Library/Fonts/STHeiti Medium.ttc", "/Library/Fonts/Arial Unicode.ttf"] {
            cands.push((p.to_string(), 0));
        }
    }
    for (p, i) in cands {
        if let Ok(b) = std::fs::read(&p) {
            return Some((b, i));
        }
    }
    None
}

fn install_fonts(ctx: &egui::Context, lang: &str) {
    let mut fonts = egui::FontDefinitions::default();
    if lang == "zh" {
        if let Some((bytes, idx)) = cjk_font() {
            fonts.font_data.insert("cjk".into(), std::sync::Arc::new(egui::FontData::from_blob(std::sync::Arc::new(bytes), idx)));
            for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts.families.entry(fam).or_default().push("cjk".into());
            }
        }
    }
    ctx.set_fonts(fonts);
}

pub fn apply_theme(ctx: &egui::Context, theme: &str) {
    let black = egui::Color32::BLACK;
    let mut oled = egui::Visuals::dark();
    oled.panel_fill = black;
    oled.window_fill = black;
    oled.extreme_bg_color = black;
    oled.faint_bg_color = egui::Color32::from_gray(12);
    oled.window_stroke = egui::Stroke::new(1.0, egui::Color32::from_gray(52));
    oled.widgets.noninteractive.bg_fill = black;
    oled.widgets.noninteractive.weak_bg_fill = black;
    oled.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, egui::Color32::from_gray(38));
    oled.widgets.inactive.bg_fill = egui::Color32::from_gray(22);
    oled.widgets.inactive.weak_bg_fill = egui::Color32::from_gray(22);
    oled.widgets.hovered.bg_fill = egui::Color32::from_gray(38);
    oled.widgets.hovered.weak_bg_fill = egui::Color32::from_gray(38);
    oled.widgets.active.bg_fill = egui::Color32::from_gray(52);
    oled.widgets.active.weak_bg_fill = egui::Color32::from_gray(52);
    oled.selection.bg_fill = egui::Color32::from_rgb(0, 70, 130);
    oled.popup_shadow = egui::epaint::Shadow::NONE;
    oled.window_shadow = egui::epaint::Shadow::NONE;
    match theme {
        "light" => ctx.set_theme(egui::ThemePreference::Light),
        "dark" => {
            ctx.set_visuals_of(egui::Theme::Dark, egui::Visuals::dark());
            ctx.set_theme(egui::ThemePreference::Dark);
        }
        "oled" => {
            ctx.set_visuals_of(egui::Theme::Dark, oled);
            ctx.set_theme(egui::ThemePreference::Dark);
        }
        _ => {
            ctx.set_visuals_of(egui::Theme::Dark, egui::Visuals::dark());
            ctx.set_theme(egui::ThemePreference::System);
        }
    }
}

fn base_style(ctx: &egui::Context) {
    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(10.0, 4.0);
        s.spacing.interact_size.y = 24.0;
        for (_, f) in s.text_styles.iter_mut() {
            f.size = (f.size * 1.05).round();
        }
    });
}

// ---------------------------------------------------------------------------------------------
// small helpers
// ---------------------------------------------------------------------------------------------
pub fn folder_name_for(archive: &Path) -> String {
    let name = archive.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let s = strip_known_ext(&name);
    if s != name && !s.is_empty() {
        return s;
    }
    archive.file_stem().map(|s| s.to_string_lossy().to_string()).filter(|s| !s.is_empty()).unwrap_or(name)
}

pub fn unique_path(dir: &Path, base: &str, ext: &str) -> PathBuf {
    let mut p = dir.join(format!("{base}{ext}"));
    let mut n = 2;
    while p.exists() {
        p = dir.join(format!("{base} ({n}){ext}"));
        n += 1;
    }
    p
}

/// (bytes, files) below the given paths; gives up (None) on huge trees so the UI never stalls.
pub fn walk_size(paths: &[PathBuf]) -> Option<(u64, u64)> {
    let (mut bytes, mut files, mut seen) = (0u64, 0u64, 0u64);
    let mut stack: Vec<PathBuf> = paths.to_vec();
    while let Some(p) = stack.pop() {
        seen += 1;
        if seen > 150_000 {
            return None;
        }
        let Ok(md) = std::fs::symlink_metadata(&p) else { continue };
        if md.is_dir() {
            if let Ok(rd) = std::fs::read_dir(&p) {
                stack.extend(rd.flatten().map(|e| e.path()));
            }
        } else {
            bytes += md.len();
            files += 1;
        }
    }
    Some((bytes, files))
}

pub fn default_archive_name(sources: &[(PathBuf, bool)], fmt: &str) -> String {
    let Some((first, _)) = sources.first() else { return String::new() };
    let dir = first.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let single = fmt_spec(fmt).map(|f| f.single).unwrap_or(false);
    let base = if sources.len() == 1 {
        let n = first.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        if single || first.is_dir() { n } else { first.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or(n) }
    } else {
        dir.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "archive".into())
    };
    let ext = fmt_spec(fmt).map(|f| f.ext).unwrap_or(".7z");
    unique_path(&dir, &base, ext).to_string_lossy().to_string()
}

pub fn open_url(u: &str) {
    open_with_default(u);
}

pub fn open_with_default<S: AsRef<std::ffi::OsStr>>(p: S) {
    let p = p.as_ref();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(p).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(p).spawn();
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("cmd").args(["/C", "start", ""]).arg(p).creation_flags(0x0800_0000).spawn();
    }
}

pub fn looks_like_archive(p: &Path) -> bool {
    let n = p.file_name().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
    ALL_EXTS.iter().any(|e| n.ends_with(e))
        || [".rar", ".iso", ".cab", ".vhd", ".vhdx", ".dmg", ".img", ".xar", ".lzh", ".lha", ".arj", ".cpio", ".rpm", ".deb", ".z", ".lzma", ".zst", ".apk", ".jar", ".msi", ".chm", ".squashfs", ".001", ".udf"].iter().any(|e| n.ends_with(e))
}

// ---------------------------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------------------------
impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, start: Startup) -> App {
        let cfg = Settings::load();
        let lang = if cfg.lang.is_empty() { i18n::system_lang() } else { cfg.lang.clone() };
        let tr = I18n::load(&lang);
        let ctx = cc.egui_ctx.clone();
        install_fonts(&ctx, &tr.lang);
        base_style(&ctx);
        apply_theme(&ctx, &cfg.theme);
        let sz = find_7z();
        let mut app = App {
            ctx,
            font_lang: tr.lang.clone(),
            cfg,
            tr,
            sz,
            sys: sysinfo(),
            arch: None,
            open_task: None,
            modals: vec![],
            pending: vec![],
            status: String::new(),
            batch_total: start.batch.len(),
            batch_done: 0,
            batch: start.batch,
            batch_mode: false,
            shot: start.shot,
            script: start.script.split(';').filter(|s| !s.is_empty()).map(|s| s.to_string()).collect(),
            script_next: 6,
            last_title: String::new(),
            prog_title: None,
            tmp_dirs: vec![],
            tmp_counter: 0,
        };
        app.set_ready_status();
        app.batch_mode = !app.batch.is_empty();
        if app.sz.is_none() {
            let m = app.tr.tf("no7z", &[&app_dir().to_string_lossy()]);
            app.msg(m);
        } else if app.batch_mode {
            app.batch.reverse();
            app.next_batch();
        } else if let Some(p) = start.open {
            app.open_archive(p, String::new(), Purpose::Normal);
        } else if !start.add.is_empty() {
            let items: Vec<(PathBuf, bool)> = start.add.into_iter().map(|p| { let d = p.is_dir(); (p, d) }).collect();
            app.push_create(items);
        } else if let Some(p) = start.extract_dialog {
            app.open_archive(p, String::new(), Purpose::ExtractDialog);
        } else if !start.hash.is_empty() {
            app.start_hash_disk(start.hash);
        }
        app
    }

    pub fn set_ready_status(&mut self) {
        self.status = match &self.sz {
            Some(s) => self.tr.tf("readyWith", &[&s.short_banner()]),
            None => self.tr.t("no7zShort"),
        };
    }
    pub fn t(&self, k: &str) -> String {
        self.tr.t(k)
    }
    pub fn tf(&self, k: &str, a: &[&str]) -> String {
        self.tr.tf(k, a)
    }
    pub fn msg(&mut self, text: String) {
        self.pending.push(Modal::Msg { text });
    }
    pub fn msg_err(&mut self, e: &Err) {
        let t = self.tr.err(e);
        self.msg(t);
    }
    pub fn set_lang(&mut self, l: &str) {
        self.cfg.lang = l.to_string();
        self.tr.set_lang(l);
        if self.font_lang != self.tr.lang {
            install_fonts(&self.ctx, &self.tr.lang);
            self.font_lang = self.tr.lang.clone();
        }
        self.cfg.save();
        self.set_ready_status();
    }
    pub fn set_theme(&mut self, th: &str) {
        self.cfg.theme = th.to_string();
        apply_theme(&self.ctx, th);
        self.cfg.save();
    }

    // ---- opening archives
    pub fn open_archive(&mut self, path: PathBuf, pw: String, purpose: Purpose) {
        let Some(sz) = self.sz.clone() else {
            let m = self.tr.tf("no7z", &[&app_dir().to_string_lossy()]);
            self.msg(m);
            return;
        };
        let (tx, rx) = mpsc::channel();
        let (p2, pw2) = (path.clone(), pw.clone());
        std::thread::spawn(move || {
            let _ = tx.send(list_archive(&sz, &p2, &pw2));
        });
        self.open_task = Some(OpenTask { rx, path, password: pw, purpose });
        self.status = self.tr.t("reading");
    }

    pub fn batch_title_for(&self, p: &Path) -> String {
        self.tf("batchItem", &[&(self.batch_done + 1).to_string(), &self.batch_total.to_string(), &p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()])
    }

    fn poll_open(&mut self) {
        let Some(t) = &self.open_task else { return };
        let Ok(r) = t.rx.try_recv() else {
            self.ctx.request_repaint_after(std::time::Duration::from_millis(50));
            return;
        };
        let t = self.open_task.take().unwrap();
        match r {
            Err(OpenError::NeedPassword { wrong }) => {
                let text = self.t(if wrong { "pwWrong" } else { "pwNeeded" });
                self.pending.push(Modal::Password { text, input: String::new(), path: t.path, purpose: t.purpose });
                self.status = self.tr.t("cancelled");
            }
            Err(OpenError::Failed(m)) => {
                let text = self.tf("arcOpenFail", &[&m]);
                self.msg(text);
                self.status = self.tr.t("openFailed");
            }
            Ok(data) => {
                let (mut size, mut packed, mut files, mut folders) = (0u64, 0u64, 0u64, 0u64);
                for e in &data.entries {
                    if e.dir {
                        folders += 1;
                    } else {
                        files += 1;
                        size += e.size.unwrap_or(0);
                    }
                    packed += e.packed.unwrap_or(0);
                }
                let fsz = data.file_size.unwrap_or(0);
                if let Purpose::BatchExtract { dest } = &t.purpose {
                    let opts = ExtractOpts { archive: t.path.clone(), dest: dest.clone(), password: t.password.clone(), keep_paths: true, mode: "aou".into(), items: vec![] };
                    let title = self.batch_title_for(&t.path);
                    self.start_progress(title, "vExtract", extract_args(&opts), JobOpts { total: Some(size), packed: Some(packed.max(fsz)), files_total: Some(files), ..Default::default() }, OnDone::Extract { dest: dest.clone() });
                    return;
                }
                if matches!(t.purpose, Purpose::BatchTest) {
                    let title = self.batch_title_for(&t.path);
                    self.start_progress(title, "vTest", test_args(&t.path, &t.password), JobOpts { total: Some(size), packed: Some(packed.max(fsz)), files_total: Some(files), ..Default::default() }, OnDone::Test);
                    return;
                }
                let tree = Tree::build(&data.entries);
                let mut expanded = HashSet::new();
                let tops = &tree.nodes[0].children;
                if tops.len() == 1 && tree.nodes[tops[0]].dir {
                    expanded.insert(tops[0]);
                }
                self.status = self.tf("statusArc", &[&files.to_string(), &fmt_size(tree.nodes[0].total), &fmt_size(fsz)]);
                self.arch = Some(Opened { path: t.path, password: t.password, data, tree, expanded, selected: HashSet::new(), anchor: None, flat: vec![], dirty: true, size, packed: packed.max(fsz), files, folders });
                if matches!(t.purpose, Purpose::ExtractDialog) {
                    self.cmd_extract();
                }
            }
        }
    }

    // ---- jobs
    pub fn start_progress(&mut self, title: String, verb: &'static str, args: Vec<String>, opts: JobOpts, on_done: OnDone) {
        let Some(sz) = &self.sz else { return };
        let job = Job::start(&sz.exe, args, opts);
        self.pending.push(Modal::Progress(Box::new(ProgressDlg::new(title, verb, job, on_done))));
    }

    pub fn start_extract(&mut self, p: ExtractParams) {
        if p.dest.as_os_str().is_empty() {
            self.msg_err(&err("E_DEST_EMPTY"));
            return;
        }
        let opts = ExtractOpts { archive: p.archive.clone(), dest: p.dest.clone(), password: p.password, keep_paths: p.keep_paths, mode: p.mode, items: p.items };
        let title = self.tr.t("tExtract");
        self.start_progress(title, "vExtract", extract_args(&opts), JobOpts { total: Some(p.total), packed: Some(p.packed), files_total: Some(p.files), ..Default::default() }, OnDone::Extract { dest: p.dest });
    }

    pub fn start_hash_disk(&mut self, paths: Vec<PathBuf>) {
        let title = self.tr.t("tHash");
        self.start_progress(title, "vHash", hash_files_args(&paths), JobOpts::default(), OnDone::Hash);
    }

    pub fn start_fetch(&mut self, node: usize) {
        let Some(a) = &self.arch else { return };
        let n = &a.tree.nodes[node];
        self.tmp_counter += 1;
        let dir = std::env::temp_dir().join(format!("u7z-{}-{}", std::process::id(), self.tmp_counter));
        let _ = std::fs::create_dir_all(&dir);
        self.tmp_dirs.push(dir.clone());
        let file = dir.join(&n.name);
        let args = fetch_args(&a.path, &a.password, &dir, &n.orig);
        let total = n.size;
        let title = self.tr.t("tFetch");
        self.start_progress(title, "vWork", args, JobOpts { total: Some(total), files_total: Some(1), ..Default::default() }, OnDone::Fetch { file });
    }

    pub fn push_create(&mut self, items: Vec<(PathBuf, bool)>) {
        let dlg = CreateDlg::new(self, items);
        self.pending.push(Modal::Create(Box::new(dlg)));
    }

    // ---- batch mode (right-click entries run these)
    pub fn next_batch(&mut self) {
        let Some(task) = self.batch.pop() else {
            self.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        };
        match task {
            Task::ExtractHere(p) => {
                let dest = p.parent().map(|d| d.to_path_buf()).unwrap_or_default();
                self.open_archive(p, String::new(), Purpose::BatchExtract { dest });
            }
            Task::ExtractTo(p) => {
                let dest = p.parent().map(|d| d.join(folder_name_for(&p))).unwrap_or_default();
                self.open_archive(p, String::new(), Purpose::BatchExtract { dest });
            }
            Task::Test(p) => self.open_archive(p, String::new(), Purpose::BatchTest),
            Task::QuickAdd { fmt, sources } => {
                let items: Vec<(PathBuf, bool)> = sources.iter().map(|p| (p.clone(), p.is_dir())).collect();
                let mut o = CreateOpts { fmt: fmt.clone(), sources, ..Default::default() };
                o.out = default_archive_name(&items, &fmt);
                o.method = methods_for(&fmt).first().map(|s| s.to_string()).unwrap_or_default();
                let _ = self.start_create(o);
            }
        }
    }

    // ---- commands
    pub fn cmd_open(&mut self) {
        let p = Picker::new(self.tr.t("pickArc"), PickMode::File, PickTarget::OpenArchive, &self.cfg.last_dir);
        self.pending.push(Modal::Picker(Box::new(p)));
    }
    pub fn cmd_new(&mut self) {
        self.push_create(vec![]);
    }
    pub fn cmd_extract(&mut self) {
        let Some(a) = &self.arch else {
            let m = self.t("openFirst");
            self.msg(m);
            return;
        };
        let dest = a.path.parent().map(|d| d.to_string_lossy().to_string()).unwrap_or_default();
        let dlg = ExtractDlg { archive: a.path.clone(), password: a.password.clone(), dest, nsel: a.selected.len(), only_sel: !a.selected.is_empty(), mode: "aoa".into(), keep: true };
        self.pending.push(Modal::Extract(Box::new(dlg)));
    }
    pub fn cmd_test(&mut self) {
        let Some(a) = &self.arch else {
            let m = self.t("openFirst");
            self.msg(m);
            return;
        };
        let args = test_args(&a.path, &a.password);
        let jo = JobOpts { total: Some(a.size), packed: Some(a.packed), files_total: Some(a.files), ..Default::default() };
        let title = self.tr.t("tTest");
        self.start_progress(title, "vTest", args, jo, OnDone::Test);
    }
    pub fn cmd_info(&mut self) {
        if self.arch.is_none() {
            let m = self.t("openFirst");
            self.msg(m);
        } else {
            self.pending.push(Modal::Props);
        }
    }
    pub fn cmd_hash(&mut self) {
        self.pending.push(Modal::HashChoose);
    }
    pub fn cmd_bench(&mut self) {
        let Some(sz) = self.sz.clone() else {
            let m = self.tr.tf("no7z", &[&app_dir().to_string_lossy()]);
            self.msg(m);
            return;
        };
        self.pending.push(Modal::Bench(Box::new(BenchDlg::new(sz, &self.sys))));
    }
    pub fn cmd_settings(&mut self) {
        self.pending.push(Modal::Settings(Box::new(SettingsDlg::default())));
    }

    /// Route the result of a picker to whatever asked for it (the dialog below it on the stack).
    pub fn route_pick(&mut self, target: PickTarget, res: PickResult, cur_dir: String) {
        if !cur_dir.is_empty() {
            self.cfg.last_dir = cur_dir;
            self.cfg.save();
        }
        match (target, res) {
            (PickTarget::OpenArchive, PickResult::Path(p)) => self.open_archive(p, String::new(), Purpose::Normal),
            (PickTarget::HashDisk, PickResult::Multi(v)) => self.start_hash_disk(v.into_iter().map(|x| x.0).collect()),
            (PickTarget::ExtractDest, PickResult::Path(p)) => {
                if let Some(Modal::Extract(d)) = self.modals.last_mut() {
                    d.dest = p.to_string_lossy().to_string();
                }
            }
            (PickTarget::CreateItems, PickResult::Multi(v)) => {
                if let Some(Modal::Create(d)) = self.modals.last_mut() {
                    d.add_items(v);
                }
            }
            (PickTarget::CreateOutDir, PickResult::Path(p)) => {
                if let Some(Modal::Create(d)) = self.modals.last_mut() {
                    d.set_out_dir(&p);
                }
            }
            _ => {}
        }
    }

    fn handle_drop(&mut self) {
        let dropped: Vec<PathBuf> = self.ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if dropped.is_empty() || !self.modals.is_empty() || self.batch_mode {
            return;
        }
        if dropped.len() == 1 && dropped[0].is_file() && looks_like_archive(&dropped[0]) {
            self.open_archive(dropped[0].clone(), String::new(), Purpose::Normal);
        } else {
            let items = dropped.into_iter().map(|p| { let d = p.is_dir(); (p, d) }).collect();
            self.push_create(items);
        }
    }

    /// Test hook: U7Z_SCRIPT="open=/x.7z;expand=1;dlg=create" runs one step every few frames.
    fn run_script(&mut self, frame: u64) {
        if self.script.is_empty() || frame < self.script_next || self.open_task.is_some() {
            return;
        }
        if !self.modals.is_empty() && !self.script[0].starts_with("close") {
            return;
        }
        let a = self.script.remove(0);
        self.script_next = frame + 8;
        let (k, v) = a.split_once('=').unwrap_or((a.as_str(), ""));
        match k {
            "open" => self.open_archive(PathBuf::from(v), String::new(), Purpose::Normal),
            "selall" => {
                if let Some(a) = &mut self.arch {
                    a.selected = a.flat.iter().map(|x| x.0).collect();
                }
            }
            "expand" => {
                if let Some(a) = &mut self.arch {
                    a.expanded = (0..a.tree.nodes.len()).filter(|&i| a.tree.nodes[i].dir).collect();
                    a.dirty = true;
                }
            }
            "close" => {
                self.modals.pop();
            }
            "add" => {
                let items: Vec<(PathBuf, bool)> = v.split('|').filter(|s| !s.is_empty()).map(|s| (PathBuf::from(s), PathBuf::from(s).is_dir())).collect();
                self.push_create(items);
            }
            "dlg" => match v {
                "create" => self.cmd_new(),
                "extract" => self.cmd_extract(),
                "props" => self.cmd_info(),
                "hash" => self.cmd_hash(),
                "bench" => self.cmd_bench(),
                "settings" => self.cmd_settings(),
                "open" => self.cmd_open(),
                "test" => self.cmd_test(),
                _ => {}
            },
            "theme" => self.set_theme(v),
            "lang" => self.set_lang(v),
            _ => {}
        }
    }

    // ---- main window
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let items: [(&str, fn(&mut App)); 7] = [
            ("bNew", App::cmd_new),
            ("bOpen", App::cmd_open),
            ("bExtract", App::cmd_extract),
            ("bTest", App::cmd_test),
            ("bInfo", App::cmd_info),
            ("bHash", App::cmd_hash),
            ("bBench", App::cmd_bench),
        ];
        let mut run: Option<fn(&mut App)> = None;
        ui.horizontal(|ui| {
            for (k, f) in items {
                let label = self.tr.t(k);
                if ui.add(egui::Button::new(label).min_size(egui::vec2(70.0, 32.0))).clicked() {
                    run = Some(f);
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(egui::Button::new(self.tr.t("settings")).min_size(egui::vec2(70.0, 32.0))).clicked() {
                    run = Some(App::cmd_settings);
                }
            });
        });
        if let Some(f) = run {
            f(self);
        }
    }

    fn archive_bar(&mut self, ui: &mut egui::Ui) {
        if let Some(a) = &self.arch {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(self.tr.t("openWith") + ":").weak());
                ui.label(egui::RichText::new(a.path.to_string_lossy()).strong());
            });
        }
    }

    fn table_ui(&mut self, ui: &mut egui::Ui) {
        let Some(a) = &mut self.arch else {
            ui.centered_and_justified(|ui| ui.weak(self.tr.t("emptyMsg")));
            return;
        };
        if a.dirty {
            a.rebuild_flat();
        }
        let tr = &self.tr;
        let heads = [tr.t("colName"), tr.t("colSize"), tr.t("colPacked"), tr.t("colModified"), tr.t("colAttr"), tr.t("colEnc")];
        let yes = tr.t("yes");
        let mut click: Option<(usize, egui::Modifiers)> = None;
        let mut dbl: Option<usize> = None;
        let mut toggle: Option<usize> = None;
        let text_col = ui.visuals().text_color();
        let weak_col = ui.visuals().weak_text_color();
        let folder_col = egui::Color32::from_rgb(232, 184, 74);
        let n = a.flat.len();
        TableBuilder::new(ui)
            .striped(true)
            .sense(egui::Sense::click())
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::remainder().at_least(200.0).clip(true))
            .column(Column::initial(90.0).at_least(40.0))
            .column(Column::initial(90.0).at_least(40.0))
            .column(Column::initial(150.0).at_least(60.0).clip(true))
            .column(Column::initial(80.0).at_least(40.0).clip(true))
            .column(Column::initial(80.0).at_least(40.0).clip(true))
            .header(24.0, |mut h| {
                for t in &heads {
                    h.col(|ui| {
                        ui.strong(t);
                    });
                }
            })
            .body(|body| {
                body.rows(22.0, n, |mut row| {
                    let (nid, depth) = a.flat[row.index()];
                    let node = &a.tree.nodes[nid];
                    row.set_selected(a.selected.contains(&nid));
                    let mut toggled = false;
                    row.col(|ui| {
                        ui.add_space(depth as f32 * 16.0);
                        let (r, resp) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::click());
                        if node.dir && !node.children.is_empty() {
                            let open = a.expanded.contains(&nid);
                            let c = r.center();
                            let pts = if open {
                                vec![c + egui::vec2(-4.0, -2.5), c + egui::vec2(4.0, -2.5), c + egui::vec2(0.0, 3.5)]
                            } else {
                                vec![c + egui::vec2(-2.5, -4.0), c + egui::vec2(-2.5, 4.0), c + egui::vec2(3.5, 0.0)]
                            };
                            ui.painter().add(egui::Shape::convex_polygon(pts, weak_col, egui::Stroke::NONE));
                            if resp.clicked() {
                                toggle = Some(nid);
                                toggled = true;
                            }
                        }
                        let (ir, _) = ui.allocate_exact_size(egui::vec2(16.0, 14.0), egui::Sense::hover());
                        if node.dir {
                            let body = egui::Rect::from_min_max(ir.min + egui::vec2(0.0, 3.0), ir.max);
                            ui.painter().rect_filled(egui::Rect::from_min_size(ir.min + egui::vec2(0.0, 1.0), egui::vec2(7.0, 4.0)), 1.0, folder_col);
                            ui.painter().rect_filled(body, 2.0, folder_col);
                        } else {
                            let pr = egui::Rect::from_min_max(ir.min + egui::vec2(2.0, 0.0), ir.max - egui::vec2(2.0, 0.0));
                            ui.painter().rect(pr, 1.5, ui.visuals().extreme_bg_color, egui::Stroke::new(1.0, weak_col), egui::StrokeKind::Inside);
                        }
                        ui.add(egui::Label::new(egui::RichText::new(&node.name).color(text_col)).truncate().selectable(false));
                    });
                    row.col(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if node.dir && node.total == 0 && node.nfiles == 0 {
                                return;
                            }
                            ui.add(egui::Label::new(fmt_size(node.size)).selectable(false));
                        });
                    });
                    row.col(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if let Some(p) = node.packed {
                                ui.add(egui::Label::new(fmt_size(p)).selectable(false));
                            }
                        });
                    });
                    row.col(|ui| {
                        ui.add(egui::Label::new(node.modified.chars().take(19).collect::<String>()).selectable(false));
                    });
                    row.col(|ui| {
                        ui.add(egui::Label::new(&node.attr).truncate().selectable(false));
                    });
                    row.col(|ui| {
                        if node.enc {
                            ui.add(egui::Label::new(&yes).selectable(false));
                        }
                    });
                    let r = row.response();
                    if !toggled {
                        if r.double_clicked() {
                            dbl = Some(nid);
                        } else if r.clicked() {
                            click = Some((row.index(), r.ctx.input(|i| i.modifiers)));
                        }
                    }
                });
            });
        if let Some((idx, m)) = click {
            let nid = a.flat[idx].0;
            if m.shift && a.anchor.is_some() {
                let (x, y) = (a.anchor.unwrap().min(idx), a.anchor.unwrap().max(idx));
                a.selected = a.flat[x..=y.min(a.flat.len() - 1)].iter().map(|v| v.0).collect();
            } else if m.command || m.ctrl {
                if !a.selected.remove(&nid) {
                    a.selected.insert(nid);
                }
                a.anchor = Some(idx);
            } else {
                a.selected.clear();
                a.selected.insert(nid);
                a.anchor = Some(idx);
            }
        }
        if let Some(nid) = toggle {
            if !a.expanded.remove(&nid) {
                a.expanded.insert(nid);
            }
            a.dirty = true;
        }
        if let Some(nid) = dbl {
            if a.tree.nodes[nid].dir {
                if !a.expanded.remove(&nid) {
                    a.expanded.insert(nid);
                }
                a.dirty = true;
            } else {
                self.start_fetch(nid);
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _f: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let frame = ctx.cumulative_pass_nr();
        self.prog_title = None;
        self.poll_open();
        self.handle_drop();
        self.run_script(frame);

        if !self.batch_mode {
            egui::Panel::top("toolbar").show(ui, |ui| {
                ui.add_space(4.0);
                self.toolbar(ui);
                self.archive_bar(ui);
                ui.add_space(2.0);
            });
            egui::Panel::bottom("status").show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(&self.status);
                    if let Some(a) = &self.arch {
                        if !a.selected.is_empty() {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(self.tr.tf("selCount", &[&a.selected.len().to_string()]));
                            });
                        }
                    }
                });
            });
            if self.modals.is_empty() {
                let (open, sel_all) = ctx.input(|i| (i.modifiers.command && i.key_pressed(egui::Key::O), i.modifiers.command && i.key_pressed(egui::Key::A)));
                if open {
                    self.cmd_open();
                }
                if sel_all {
                    if let Some(a) = &mut self.arch {
                        a.selected = a.flat.iter().map(|x| x.0).collect();
                    }
                }
            }
            egui::CentralPanel::default().show(ui, |ui| {
                self.table_ui(ui);
            });
        }

        self.run_modals(ui);

        // batch: when nothing is pending go on to the next task
        if self.batch_mode && self.modals.is_empty() && self.pending.is_empty() && self.open_task.is_none() && frame > 3 {
            self.batch_done += 1;
            self.next_batch();
        }

        // window title
        let title = match (&self.prog_title, &self.arch) {
            (Some(t), _) => format!("{t} — Universal 7-Zip GUI"),
            (None, Some(a)) if !self.batch_mode => format!("{} — Universal 7-Zip GUI", a.path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()),
            _ => "Universal 7-Zip GUI".into(),
        };
        if title != self.last_title {
            self.last_title = title.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
        }

        // test hook: screenshot and exit
        if let Some((p, at)) = self.shot.clone() {
            if frame >= at && self.script.is_empty() && self.open_task.is_none() {
                self.shot = None;
                ctx.request_screenshot(move |img| {
                    let f = std::fs::File::create(&p).unwrap();
                    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), img.width() as u32, img.height() as u32);
                    enc.set_color(png::ColorType::Rgba);
                    enc.set_depth(png::BitDepth::Eight);
                    let mut w = enc.write_header().unwrap();
                    let data: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
                    w.write_image_data(&data).unwrap();
                    w.finish().unwrap();
                    std::process::exit(0);
                });
            }
            ctx.request_repaint();
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        for d in &self.tmp_dirs {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}
