//! 7-Zip console wrapper: finding the executable, running jobs with progress parsing,
//! archive listing, create options, hashes, benchmark. No GUI code here.
use regex::Regex;
use std::collections::VecDeque;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

pub const IS_WIN: bool = cfg!(windows);
pub const IS_MAC: bool = cfg!(target_os = "macos");

// ---------------------------------------------------------------------------------------------
// errors (translated by the GUI through the "e.<code>" keys)
// ---------------------------------------------------------------------------------------------
#[derive(Debug, Clone)]
pub struct Err {
    pub code: &'static str,
    pub arg: Option<String>,
}
pub fn err(code: &'static str) -> Err {
    Err { code, arg: None }
}
pub fn err_arg(code: &'static str, arg: impl Into<String>) -> Err {
    Err { code, arg: Some(arg.into()) }
}

// ---------------------------------------------------------------------------------------------
// format tables
// ---------------------------------------------------------------------------------------------
pub struct FmtSpec {
    pub key: &'static str,
    pub t: &'static str,
    pub ext: &'static str,
    pub single: bool,
    pub tar: bool,
}
pub const FORMATS: &[FmtSpec] = &[
    FmtSpec { key: "7z", t: "7z", ext: ".7z", single: false, tar: false },
    FmtSpec { key: "zip", t: "zip", ext: ".zip", single: false, tar: false },
    FmtSpec { key: "tar", t: "tar", ext: ".tar", single: false, tar: false },
    FmtSpec { key: "gzip", t: "gzip", ext: ".gz", single: true, tar: false },
    FmtSpec { key: "bzip2", t: "bzip2", ext: ".bz2", single: true, tar: false },
    FmtSpec { key: "xz", t: "xz", ext: ".xz", single: true, tar: false },
    FmtSpec { key: "wim", t: "wim", ext: ".wim", single: false, tar: false },
    FmtSpec { key: "tar.gz", t: "gzip", ext: ".tar.gz", single: false, tar: true },
    FmtSpec { key: "tar.bz2", t: "bzip2", ext: ".tar.bz2", single: false, tar: true },
    FmtSpec { key: "tar.xz", t: "xz", ext: ".tar.xz", single: false, tar: true },
];
pub fn fmt_spec(key: &str) -> Option<&'static FmtSpec> {
    FORMATS.iter().find(|f| f.key == key)
}
pub fn fmt_label(key: &str) -> &'static str {
    match key {
        "gzip" => "gzip (.gz)",
        "bzip2" => "bzip2 (.bz2)",
        other => FORMATS.iter().find(|f| f.key == other).map(|f| f.key).unwrap_or("?"),
    }
}
pub fn methods_for(fmt: &str) -> &'static [&'static str] {
    match fmt {
        "7z" => &["LZMA2", "LZMA", "PPMd", "BZip2", "Deflate", "Deflate64", "Copy"],
        "zip" => &["Deflate", "Deflate64", "BZip2", "LZMA", "PPMd", "Copy"],
        "gzip" | "tar.gz" => &["Deflate"],
        "bzip2" | "tar.bz2" => &["BZip2"],
        "xz" | "tar.xz" => &["LZMA2"],
        _ => &[],
    }
}
pub fn dict_options(method: &str) -> &'static [&'static str] {
    match method {
        "LZMA2" | "LZMA" => &[
            "64k", "256k", "1m", "2m", "3m", "4m", "6m", "8m", "12m", "16m", "24m", "32m", "48m", "64m", "96m",
            "128m", "192m", "256m", "384m", "512m", "768m", "1024m", "1536m",
        ],
        "PPMd" => &["1m", "2m", "4m", "8m", "16m", "32m", "64m", "128m", "256m", "512m", "1024m"],
        "BZip2" => &["100k", "200k", "300k", "400k", "500k", "600k", "700k", "800k", "900k"],
        _ => &[],
    }
}
pub fn word_options(method: &str) -> &'static [u32] {
    match method {
        "LZMA2" | "LZMA" => &[8, 12, 16, 24, 32, 48, 64, 96, 128, 192, 256, 273],
        "PPMd" => &[2, 3, 4, 5, 6, 8, 12, 16, 24, 32],
        "Deflate" => &[8, 12, 16, 24, 32, 48, 64, 96, 128, 192, 258],
        "Deflate64" => &[8, 12, 16, 24, 32, 48, 64, 96, 128, 192, 257],
        _ => &[],
    }
}
pub const SOLID_OPTIONS: &[&str] = &[
    "off", "1m", "2m", "4m", "8m", "16m", "32m", "64m", "128m", "256m", "512m", "1g", "2g", "4g", "8g", "16g", "32g", "64g", "on",
];
pub fn dict_label(v: &str) -> String {
    let (n, u) = v.split_at(v.len() - 1);
    match u {
        "k" | "K" => format!("{n} KB"),
        "m" | "M" => format!("{n} MB"),
        "g" | "G" => format!("{n} GB"),
        _ => v.to_string(),
    }
}
const LEVELS: [u32; 6] = [0, 1, 3, 5, 7, 9];
const UPDATABLE: [&str; 4] = ["7z", "zip", "tar", "wim"];

// ---------------------------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------------------------
static CTRL_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\x00-\x08\x0b-\x1f\x7f]").unwrap());
static PROG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(\d{1,3})%(?:\s+(\d+))?(?:\s+(?:([-+UDRT.=!])\s+)?(.+?))?\s*$").unwrap());
static PART_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*(\d{1,3})%(?:\s+(\d+)\s)?").unwrap());
static FILE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^([-+UDRT.=!])\s+(.+)$").unwrap());
static SUM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|:)\s*(?:\d+ folders?,\s*)?(\d+) files?,\s*(\d+) bytes").unwrap());
static HASH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\S+)\s+for (data and names|data):\s*(\S+)\s*$").unwrap());
static RULE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^-+(?: +-+)+\s*$").unwrap());

fn cmd_new(exe: &Path) -> Command {
    let mut c = Command::new(exe);
    c.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        c.arg("-sccUTF-8");
    }
    let loc = std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LC_CTYPE"))
        .or_else(|_| std::env::var("LANG"))
        .unwrap_or_default();
    if !loc.to_lowercase().contains("utf") {
        c.env("LC_ALL", "C.UTF-8");
    }
    c
}

pub fn to_u64(s: Option<&str>) -> Option<u64> {
    s.and_then(|v| v.trim().parse().ok())
}

/// Minimal shell-like splitter for the "Parameters" field (quotes supported).
pub fn split_params(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut have = false;
    for ch in s.chars() {
        match quote {
            Some(q) => {
                if ch == q {
                    quote = None;
                } else {
                    cur.push(ch);
                }
            }
            None => {
                if ch == '"' || ch == '\'' {
                    quote = Some(ch);
                    have = true;
                } else if ch.is_whitespace() {
                    if have || !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                        have = false;
                    }
                } else {
                    cur.push(ch);
                }
            }
        }
    }
    if have || !cur.is_empty() {
        out.push(cur);
    }
    out
}

// ---------------------------------------------------------------------------------------------
// finding 7-Zip
// ---------------------------------------------------------------------------------------------
#[derive(Clone, Debug)]
pub struct SevenZip {
    pub exe: PathBuf,
    pub banner: String,
}
impl SevenZip {
    pub fn short_banner(&self) -> String {
        match self.banner.find(" : Copyright") {
            Some(i) => self.banner[..i].trim().to_string(),
            None => self.banner.clone(),
        }
    }
}

fn candidates() -> Vec<&'static str> {
    if IS_WIN {
        vec!["7zz.exe", "7zzs.exe", "7z.exe", "7za.exe", "7zr.exe"]
    } else {
        vec!["7zz", "7zzs", "7z", "7za", "7zr"]
    }
}

fn probe(path: &Path) -> Option<String> {
    let out = cmd_new(path).stdout(Stdio::piped()).stderr(Stdio::piped()).output().ok()?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    text.lines().find(|l| l.contains("7-Zip")).map(|l| l.trim().to_string())
}

pub fn app_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| std::fs::canonicalize(&p).ok().or(Some(p)))
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| p.is_file())
}

fn known_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if IS_WIN {
        for k in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"] {
            if let Some(d) = std::env::var_os(k) {
                v.push(PathBuf::from(d).join("7-Zip"));
            }
        }
    } else if IS_MAC {
        for d in ["/opt/homebrew/bin", "/usr/local/bin", "/Applications/7-Zip"] {
            v.push(PathBuf::from(d));
        }
    } else {
        for d in ["/usr/local/bin", "/usr/lib/7zip", "/usr/libexec/7zip"] {
            v.push(PathBuf::from(d));
        }
    }
    v
}

/// 1) U7Z_7Z env, 2) next to this program (and ./7zip), 3) PATH, 4) known install folders.
pub fn find_7z() -> Option<SevenZip> {
    if let Some(p) = std::env::var_os("U7Z_7Z") {
        let p = PathBuf::from(p);
        if let Some(b) = probe(&p) {
            return Some(SevenZip { exe: p, banner: b });
        }
    }
    let here = app_dir();
    let mut dirs = vec![here.clone(), here.join("7zip"), here.join("7-Zip")];
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd);
    }
    for name in candidates() {
        for d in &dirs {
            let p = d.join(name);
            if p.is_file() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(md) = std::fs::metadata(&p) {
                        let mut perm = md.permissions();
                        if perm.mode() & 0o111 == 0 {
                            perm.set_mode(perm.mode() | 0o111);
                            let _ = std::fs::set_permissions(&p, perm);
                        }
                    }
                }
                if let Some(b) = probe(&p) {
                    return Some(SevenZip { exe: p, banner: b });
                }
            }
        }
    }
    for name in candidates() {
        if let Some(p) = which(name) {
            if let Some(b) = probe(&p) {
                return Some(SevenZip { exe: p, banner: b });
            }
        }
    }
    for d in known_dirs() {
        for name in candidates() {
            let p = d.join(name);
            if p.is_file() {
                if let Some(b) = probe(&p) {
                    return Some(SevenZip { exe: p, banner: b });
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------------------------
// jobs
// ---------------------------------------------------------------------------------------------
#[derive(Default, Clone)]
pub struct JobOpts {
    pub total: Option<u64>,
    pub packed: Option<u64>,
    pub files_total: Option<u64>,
    pub out: Option<PathBuf>,
    pub parse_sum: bool,
    pub use_pty: bool,
    pub pipe: Option<Vec<String>>,
}

#[derive(Clone, Default, Debug)]
pub struct Snapshot {
    pub pct: Option<u32>,
    pub cur: String,
    pub files: Option<u64>,
    pub files_total: Option<u64>,
    pub el: f64,
    pub rem: Option<f64>,
    pub total: Option<u64>,
    pub processed: Option<f64>,
    pub speed: Option<f64>,
    pub comp: Option<f64>,
    pub ratio: Option<f64>,
    pub done: bool,
    pub rc: Option<i32>,
    pub cancelled: bool,
    pub paused: bool,
    pub background: bool,
}

struct State {
    lines: Vec<String>,
    pct: Option<u32>,
    files: Option<u64>,
    cur: String,
    rc: Option<i32>,
    done: bool,
    cancelled: bool,
    paused: bool,
    background: bool,
    start: Instant,
    end: Option<Instant>,
    pause_at: Option<Instant>,
    paused_total: f64,
    samples: VecDeque<(f64, f64)>,
    total: Option<u64>,
    packed: Option<u64>,
    files_total: Option<u64>,
    out: Option<PathBuf>,
    parse_sum: bool,
    use_pty: bool,
}

impl State {
    fn set_progress(&mut self, pct: u32, files: Option<&str>) {
        self.pct = Some(pct.min(100));
        if let Some(f) = files.and_then(|f| f.parse::<u64>().ok()) {
            if f > 0 {
                self.files = Some(f);
            }
        }
    }
    fn piece(&mut self, bytes: &[u8]) {
        let s = String::from_utf8_lossy(bytes);
        let line = CTRL_RE.replace_all(&s, "").trim_end().to_string();
        let text = line.trim();
        if text.is_empty() {
            return;
        }
        if let Some(m) = PROG_RE.captures(text) {
            let pct: u32 = m[1].parse().unwrap_or(0);
            self.set_progress(pct, m.get(2).map(|x| x.as_str()));
            if let Some(p) = m.get(4) {
                self.cur = p.as_str().to_string();
            }
            return;
        }
        if self.parse_sum {
            if let Some(m) = SUM_RE.captures(text) {
                self.files_total = m[1].parse().ok();
                self.total = m[2].parse().ok();
            }
        }
        if !self.use_pty {
            if let Some(m) = FILE_RE.captures(text) {
                self.cur = m[2].to_string();
                return;
            }
        }
        if self.lines.len() < 5000 {
            self.lines.push(line);
        }
    }
    fn partial(&mut self, buf: &[u8]) {
        let s = String::from_utf8_lossy(buf);
        if let Some(m) = PART_RE.captures(&s) {
            let pct: u32 = m[1].parse().unwrap_or(0);
            self.set_progress(pct, m.get(2).map(|x| x.as_str()));
        }
    }
    fn elapsed(&self) -> f64 {
        let now = self.end.unwrap_or_else(Instant::now);
        let paused_now = self.pause_at.map(|p| now.duration_since(p).as_secs_f64()).unwrap_or(0.0);
        (now.duration_since(self.start).as_secs_f64() - self.paused_total - paused_now).max(0.0)
    }
}

pub struct Job {
    st: Arc<Mutex<State>>,
    children: Arc<Mutex<Vec<Child>>>,
}

fn feed(st: &Arc<Mutex<State>>, mut rd: impl Read, plain: bool) {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 65536];
    loop {
        let n = match rd.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        buf.extend_from_slice(&chunk[..n]);
        let mut start = 0usize;
        let mut pieces: Vec<(usize, usize)> = Vec::new();
        for (i, b) in buf.iter().enumerate() {
            if *b == b'\r' || *b == b'\n' || *b == 8 {
                if i > start {
                    pieces.push((start, i));
                }
                start = i + 1;
            }
        }
        let mut s = st.lock().unwrap();
        for (a, b) in &pieces {
            if plain {
                let t = String::from_utf8_lossy(&buf[*a..*b]);
                let t = CTRL_RE.replace_all(&t, "").trim_end().to_string();
                if !t.trim().is_empty() && s.lines.len() < 5000 {
                    s.lines.push(t);
                }
            } else {
                s.piece(&buf[*a..*b]);
            }
        }
        let rest = buf[start..].to_vec();
        if !plain {
            s.partial(&rest);
        }
        drop(s);
        buf = rest;
    }
    if !buf.is_empty() {
        let mut s = st.lock().unwrap();
        if plain {
            let t = String::from_utf8_lossy(&buf);
            let t = CTRL_RE.replace_all(&t, "").trim_end().to_string();
            if !t.trim().is_empty() && s.lines.len() < 5000 {
                s.lines.push(t);
            }
        } else {
            s.piece(&buf);
        }
    }
}

#[cfg(unix)]
fn open_pty() -> Option<(std::fs::File, std::process::Stdio, std::process::Stdio)> {
    use std::os::fd::FromRawFd;
    let mut master: libc::c_int = 0;
    let mut slave: libc::c_int = 0;
    let rc = unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) };
    if rc != 0 {
        return None;
    }
    let s2 = unsafe { libc::dup(slave) };
    unsafe {
        Some((
            std::fs::File::from_raw_fd(master),
            Stdio::from(std::fs::File::from_raw_fd(slave)),
            Stdio::from(std::fs::File::from_raw_fd(s2)),
        ))
    }
}

impl Job {
    pub fn start(exe: &Path, args: Vec<String>, o: JobOpts) -> Job {
        let st = Arc::new(Mutex::new(State {
            lines: Vec::new(),
            pct: None,
            files: None,
            cur: String::new(),
            rc: None,
            done: false,
            cancelled: false,
            paused: false,
            background: false,
            start: Instant::now(),
            end: None,
            pause_at: None,
            paused_total: 0.0,
            samples: VecDeque::new(),
            total: o.total,
            packed: o.packed,
            files_total: o.files_total,
            out: o.out.clone(),
            parse_sum: o.parse_sum,
            use_pty: o.use_pty && !IS_WIN,
        }));
        let children = Arc::new(Mutex::new(Vec::<Child>::new()));
        let job = Job { st: st.clone(), children: children.clone() };
        let exe = exe.to_path_buf();
        std::thread::spawn(move || run_job(exe, args, o, st, children));
        job
    }

    pub fn cancel(&self) {
        {
            let mut s = self.st.lock().unwrap();
            s.cancelled = true;
        }
        if self.st.lock().unwrap().paused {
            self.set_paused(false);
        }
        for c in self.children.lock().unwrap().iter_mut() {
            if matches!(c.try_wait(), Ok(None)) {
                #[cfg(unix)]
                unsafe {
                    libc::kill(c.id() as i32, libc::SIGTERM);
                }
                #[cfg(not(unix))]
                {
                    let _ = c.kill();
                }
            }
        }
    }

    pub fn set_paused(&self, on: bool) -> bool {
        let mut kids = self.children.lock().unwrap();
        let mut s = self.st.lock().unwrap();
        if s.done {
            return false;
        }
        if on == s.paused {
            return true;
        }
        for c in kids.iter_mut() {
            if !matches!(c.try_wait(), Ok(None)) {
                continue;
            }
            #[cfg(unix)]
            unsafe {
                libc::kill(c.id() as i32, if on { libc::SIGSTOP } else { libc::SIGCONT });
            }
            #[cfg(windows)]
            winapi::suspend(&*c, on);
        }
        if on {
            s.paused = true;
            s.pause_at = Some(Instant::now());
        } else {
            if let Some(p) = s.pause_at.take() {
                s.paused_total += p.elapsed().as_secs_f64();
            }
            s.paused = false;
        }
        true
    }

    /// Windows' "Background" button: lowers the priority of the 7-Zip process.
    pub fn set_background(&self, on: bool) -> bool {
        let mut kids = self.children.lock().unwrap();
        let mut ok = true;
        for c in kids.iter_mut() {
            if !matches!(c.try_wait(), Ok(None)) {
                continue;
            }
            #[cfg(windows)]
            {
                ok = winapi::priority(&*c, on) && ok;
            }
            #[cfg(unix)]
            {
                let pid = c.id();
                let tids: Vec<u32> = std::fs::read_dir(format!("/proc/{pid}/task"))
                    .map(|d| d.filter_map(|e| e.ok()).filter_map(|e| e.file_name().to_string_lossy().parse().ok()).collect())
                    .unwrap_or_else(|_| vec![pid]);
                for tid in tids {
                    let r = unsafe { libc::setpriority(libc::PRIO_PROCESS as _, tid as _, if on { 10 } else { 0 }) };
                    if r != 0 {
                        ok = false;
                    }
                }
            }
        }
        let mut s = self.st.lock().unwrap();
        s.background = (ok && on) || (!ok && !on);
        ok
    }

    pub fn is_done(&self) -> bool {
        self.st.lock().unwrap().done
    }
    pub fn lines(&self) -> Vec<String> {
        self.st.lock().unwrap().lines.clone()
    }
    pub fn text(&self) -> String {
        self.st.lock().unwrap().lines.join("\n")
    }
    pub fn rc(&self) -> Option<i32> {
        self.st.lock().unwrap().rc
    }

    pub fn snapshot(&self) -> Snapshot {
        let mut s = self.st.lock().unwrap();
        let done = s.done;
        let el = s.elapsed();
        let mut pct = s.pct;
        if done && matches!(s.rc, Some(0) | Some(1)) && !s.cancelled && s.total.is_some() {
            pct = Some(100);
        }
        let total = s.total;
        let processed = match (total, pct) {
            (Some(t), Some(p)) => Some(t as f64 * p as f64 / 100.0),
            _ => None,
        };
        let mut speed = None;
        if let Some(pr) = processed {
            let changed = s.samples.back().map(|x| x.1 != pr).unwrap_or(true);
            if changed {
                s.samples.push_back((el, pr));
            }
            while s.samples.len() > 1 && el - s.samples[1].0 >= 8.0 {
                s.samples.pop_front();
            }
            let first = *s.samples.front().unwrap();
            if el - first.0 >= 1.0 {
                speed = Some(((pr - first.1) / (el - first.0)).max(0.0));
            } else if el > 0.3 {
                speed = Some(pr / el);
            }
        }
        let rem = match (speed, total, processed) {
            (Some(sp), Some(t), Some(pr)) if sp > 0.0 => Some(((t as f64 - pr) / sp).max(0.0)),
            _ => None,
        };
        let mut comp = None;
        if let Some(out) = &s.out {
            let mut best: Option<u64> = None;
            let mut name1 = out.clone().into_os_string();
            name1.push(".tmp");
            let mut name2 = out.clone().into_os_string();
            name2.push(".001");
            for p in [out.clone(), PathBuf::from(name1), PathBuf::from(name2)] {
                if let Ok(md) = std::fs::metadata(&p) {
                    best = Some(best.map_or(md.len(), |b| b.max(md.len())));
                }
            }
            comp = best.map(|b| b as f64);
        } else if let (Some(pk), Some(p)) = (s.packed, pct) {
            comp = Some(pk as f64 * p as f64 / 100.0);
        }
        let ratio = match (comp, processed) {
            (Some(c), Some(p)) if p > 0.0 => Some(c / p * 100.0),
            _ => None,
        };
        Snapshot {
            pct,
            cur: s.cur.clone(),
            files: s.files,
            files_total: s.files_total,
            el,
            rem,
            total,
            processed,
            speed,
            comp,
            ratio,
            done,
            rc: if done { s.rc } else { None },
            cancelled: s.cancelled,
            paused: s.paused,
            background: s.background,
        }
    }
}

fn finish(st: &Arc<Mutex<State>>, rc: i32) {
    let mut s = st.lock().unwrap();
    s.rc = Some(rc);
    s.end = Some(Instant::now());
    s.done = true;
}

fn fail_start(st: &Arc<Mutex<State>>, msg: String) {
    st.lock().unwrap().lines.push(msg);
    finish(st, -1);
}

fn run_job(exe: PathBuf, args: Vec<String>, o: JobOpts, st: Arc<Mutex<State>>, children: Arc<Mutex<Vec<Child>>>) {
    let mut readers: Vec<std::thread::JoinHandle<()>> = Vec::new();
    let mut master_file: Option<std::fs::File> = None;
    if let Some(pipe_args) = o.pipe.clone() {
        let mut c1 = cmd_new(&exe);
        c1.args(&args).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut p1 = match c1.spawn() {
            Ok(p) => p,
            Err(e) => return fail_start(&st, e.to_string()),
        };
        let p1_out = p1.stdout.take().unwrap();
        let p1_err = p1.stderr.take().unwrap();
        let mut c2 = cmd_new(&exe);
        c2.args(&pipe_args).stdin(Stdio::from(p1_out)).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut p2 = match c2.spawn() {
            Ok(p) => p,
            Err(e) => {
                let _ = p1.kill();
                return fail_start(&st, e.to_string());
            }
        };
        let p2_out = p2.stdout.take().unwrap();
        let p2_err = p2.stderr.take().unwrap();
        {
            let mut k = children.lock().unwrap();
            k.push(p1);
            k.push(p2);
        }
        let s1 = st.clone();
        readers.push(std::thread::spawn(move || feed(&s1, p1_err, false)));
        let s2 = st.clone();
        readers.push(std::thread::spawn(move || feed(&s2, p2_out, true)));
        let s3 = st.clone();
        readers.push(std::thread::spawn(move || feed(&s3, p2_err, true)));
    } else {
        let mut c = cmd_new(&exe);
        c.args(&args);
        #[cfg(unix)]
        let pty = if st.lock().unwrap().use_pty { open_pty() } else { None };
        #[cfg(not(unix))]
        let pty: Option<(std::fs::File, Stdio, Stdio)> = None;
        if let Some((m, so, se)) = pty {
            c.stdout(so).stderr(se);
            master_file = Some(m);
        } else {
            c.stdout(Stdio::piped()).stderr(Stdio::piped());
        }
        let mut child = match c.spawn() {
            Ok(p) => p,
            Err(e) => return fail_start(&st, e.to_string()),
        };
        drop(c); // closes our copies of the pty slave
        if let Some(m) = master_file.take() {
            let s1 = st.clone();
            readers.push(std::thread::spawn(move || feed(&s1, m, false)));
        } else {
            let out = child.stdout.take().unwrap();
            let err = child.stderr.take().unwrap();
            let s1 = st.clone();
            readers.push(std::thread::spawn(move || feed(&s1, out, false)));
            let s2 = st.clone();
            readers.push(std::thread::spawn(move || feed(&s2, err, false)));
        }
        children.lock().unwrap().push(child);
    }
    // wait for all processes
    let mut rc = 0;
    loop {
        let mut running = false;
        {
            let mut kids = children.lock().unwrap();
            for c in kids.iter_mut() {
                match c.try_wait() {
                    Ok(None) => running = true,
                    Ok(Some(status)) => rc = rc.max(status.code().unwrap_or(-1)),
                    Err(_) => {}
                }
            }
        }
        if !running {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    // recompute rc (try_wait result consumed above only when it returned Some once; do it robustly)
    {
        let mut kids = children.lock().unwrap();
        for c in kids.iter_mut() {
            if let Ok(Some(status)) = c.try_wait() {
                rc = rc.max(status.code().unwrap_or(-1));
            }
        }
    }
    // a pty master only reaches EOF when the slave closes; give readers a moment, never hang
    let deadline = Instant::now() + Duration::from_secs(3);
    for r in readers {
        while !r.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if r.is_finished() {
            let _ = r.join();
        }
    }
    finish(&st, rc);
}

#[cfg(windows)]
mod winapi {
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtSuspendProcess(h: isize) -> i32;
        fn NtResumeProcess(h: isize) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetPriorityClass(h: isize, class: u32) -> i32;
    }
    pub fn suspend(c: &Child, on: bool) {
        let h = c.as_raw_handle() as isize;
        unsafe {
            if on {
                NtSuspendProcess(h);
            } else {
                NtResumeProcess(h);
            }
        }
    }
    pub fn priority(c: &Child, low: bool) -> bool {
        unsafe { SetPriorityClass(c.as_raw_handle() as isize, if low { 0x4000 } else { 0x20 }) != 0 }
    }
}

/// Runs 7-Zip to completion and returns (exit code, combined text). Used for quick commands.
pub fn run_capture(exe: &Path, args: &[String]) -> (i32, String) {
    match cmd_new(exe).args(args).stdout(Stdio::piped()).stderr(Stdio::piped()).output() {
        Ok(o) => {
            let mut t = String::from_utf8_lossy(&o.stdout).to_string();
            t.push_str(&String::from_utf8_lossy(&o.stderr));
            (o.status.code().unwrap_or(-1), t)
        }
        Err(e) => (-1, e.to_string()),
    }
}

// ---------------------------------------------------------------------------------------------
// archive listing
// ---------------------------------------------------------------------------------------------
#[derive(Clone, Debug)]
pub struct Entry {
    pub path: String,
    pub key: String,
    pub dir: bool,
    pub size: Option<u64>,
    pub packed: Option<u64>,
    pub modified: String,
    pub attr: String,
    pub enc: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ArchiveData {
    pub entries: Vec<Entry>,
    pub file_size: Option<u64>,
    pub props: Vec<(String, String)>,
}

pub enum OpenError {
    NeedPassword { wrong: bool },
    Failed(String),
}

pub fn parse_slt(text: &str, default_name: &str) -> Vec<std::collections::HashMap<String, String>> {
    let mut entries = Vec::new();
    let mut cur: std::collections::HashMap<String, String> = Default::default();
    let mut in_entries = false;
    let flush = |cur: &mut std::collections::HashMap<String, String>, entries: &mut Vec<_>| {
        if !cur.is_empty() && !cur.contains_key("Path") && !default_name.is_empty() {
            cur.insert("Path".into(), default_name.to_string());
        }
        if cur.contains_key("Path") {
            entries.push(cur.clone());
        }
        cur.clear();
    };
    for line in text.lines() {
        if !in_entries {
            if line.trim() == "----------" {
                in_entries = true;
            }
            continue;
        }
        if line.trim().is_empty() {
            flush(&mut cur, &mut entries);
            continue;
        }
        if let Some((k, v)) = line.split_once(" = ") {
            cur.insert(k.trim().to_string(), v.to_string());
        }
    }
    flush(&mut cur, &mut entries);
    entries
}

pub fn parse_props(text: &str) -> Vec<(String, String)> {
    let mut props = Vec::new();
    let mut started = false;
    for line in text.lines() {
        if line.trim() == "--" {
            started = true;
            continue;
        }
        if started {
            if line.trim().is_empty() {
                break;
            }
            if let Some((k, v)) = line.split_once(" = ") {
                props.push((k.trim().to_string(), v.to_string()));
            }
        }
    }
    props
}

pub fn list_archive(sz: &SevenZip, path: &Path, pw: &str) -> Result<ArchiveData, OpenError> {
    let mut args: Vec<String> = vec!["l".into(), "-slt".into()];
    if !pw.is_empty() {
        args.push(format!("-p{pw}"));
    }
    args.push("--".into());
    args.push(path.to_string_lossy().to_string());
    let (rc, text) = run_capture(&sz.exe, &args);
    if rc >= 2 || rc < 0 {
        let low = text.to_lowercase();
        if low.contains("password") || low.contains("encrypted") {
            return Err(OpenError::NeedPassword { wrong: !pw.is_empty() });
        }
        let tail: Vec<&str> = text.trim().lines().collect();
        let tail = tail[tail.len().saturating_sub(8)..].join("\n");
        return Err(OpenError::Failed(if tail.is_empty() { "?".into() } else { tail }));
    }
    let base = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let dflt = Path::new(&base).file_stem().map(|s| s.to_string_lossy().to_string()).filter(|s| !s.is_empty()).unwrap_or(base.clone());
    let mut entries = Vec::new();
    for e in parse_slt(&text, &dflt) {
        let p = e.get("Path").cloned().unwrap_or_default();
        let attr = e.get("Attributes").cloned().unwrap_or_default();
        entries.push(Entry {
            key: if IS_WIN { p.replace('\\', "/") } else { p.clone() },
            path: p,
            dir: e.get("Folder").map(|s| s == "+").unwrap_or(false) || attr.starts_with('D'),
            size: to_u64(e.get("Size").map(|s| s.as_str())),
            packed: to_u64(e.get("Packed Size").map(|s| s.as_str())),
            modified: e.get("Modified").map(|s| s.chars().take(19).collect()).unwrap_or_default(),
            attr,
            enc: e.get("Encrypted").map(|s| s == "+").unwrap_or(false),
        });
    }
    Ok(ArchiveData { entries, file_size: std::fs::metadata(path).ok().map(|m| m.len()), props: parse_props(&text) })
}

// ---------------------------------------------------------------------------------------------
// extract / test / hash command builders
// ---------------------------------------------------------------------------------------------
fn pw_args(pw: &str) -> Vec<String> {
    if pw.is_empty() { vec![] } else { vec![format!("-p{pw}")] }
}

pub struct ExtractOpts {
    pub archive: PathBuf,
    pub dest: PathBuf,
    pub password: String,
    pub keep_paths: bool,
    pub mode: String, // aoa | aos | aou | aot
    pub items: Vec<(String, bool)>,
}

pub fn item_args(items: &[(String, bool)]) -> Vec<String> {
    let sep = if IS_WIN { "\\" } else { "/" };
    let mut out = Vec::new();
    for (p, d) in items {
        if p.is_empty() {
            continue;
        }
        out.push(p.clone());
        if *d {
            out.push(format!("{}{}*", p.trim_end_matches(['/', '\\']), sep));
        }
    }
    out
}

pub fn extract_args(o: &ExtractOpts) -> Vec<String> {
    let mode = if ["aoa", "aos", "aou", "aot"].contains(&o.mode.as_str()) { o.mode.as_str() } else { "aoa" };
    let mut a: Vec<String> = vec![
        if o.keep_paths { "x".into() } else { "e".into() },
        format!("-o{}", o.dest.to_string_lossy()),
        "-y".into(),
        format!("-{mode}"),
        "-bsp1".into(),
        "-bb1".into(),
    ];
    a.extend(pw_args(&o.password));
    a.push("--".into());
    a.push(o.archive.to_string_lossy().to_string());
    a.extend(item_args(&o.items));
    a
}

pub fn test_args(archive: &Path, pw: &str) -> Vec<String> {
    let mut a: Vec<String> = vec!["t".into(), "-bsp1".into(), "-bb1".into()];
    a.extend(pw_args(pw));
    a.push("--".into());
    a.push(archive.to_string_lossy().to_string());
    a
}

pub fn hash_archive_args(archive: &Path, pw: &str, items: &[(String, bool)]) -> Vec<String> {
    let mut a: Vec<String> = vec!["t".into(), "-scrc*".into(), "-bsp1".into(), "-bb1".into()];
    a.extend(pw_args(pw));
    a.push("--".into());
    a.push(archive.to_string_lossy().to_string());
    a.extend(item_args(items));
    a
}

pub fn hash_files_args(paths: &[PathBuf]) -> Vec<String> {
    let mut a: Vec<String> = vec!["h".into(), "-scrc*".into(), "-bsp1".into(), "--".into()];
    a.extend(paths.iter().map(|p| p.to_string_lossy().to_string()));
    a
}

pub fn fetch_args(archive: &Path, pw: &str, tmp: &Path, path: &str) -> Vec<String> {
    let mut a: Vec<String> = vec!["e".into(), format!("-o{}", tmp.to_string_lossy()), "-y".into(), "-aoa".into(), "-bsp1".into()];
    a.extend(pw_args(pw));
    a.push("--".into());
    a.push(archive.to_string_lossy().to_string());
    a.push(path.to_string());
    a
}

pub fn bench_args(threads: u32, dict_log: u32, passes: u32) -> Vec<String> {
    vec!["b".into(), format!("-mmt={threads}"), format!("-md={dict_log}"), passes.to_string()]
}

// ---------------------------------------------------------------------------------------------
// hash result parsing
// ---------------------------------------------------------------------------------------------
#[derive(Default, Clone, Debug)]
pub struct HashRow {
    pub name: String,
    pub data: Option<String>,
    pub names: Option<String>,
}
#[derive(Default, Clone, Debug)]
pub struct HashItem {
    pub name: String,
    pub size: Option<u64>,
    pub hashes: Vec<(String, String)>,
}
#[derive(Default, Clone, Debug)]
pub struct HashResult {
    pub size: Option<u64>,
    pub files: Option<u64>,
    pub rows: Vec<HashRow>,
    pub items: Vec<HashItem>,
}

pub fn parse_hash(text: &str) -> HashResult {
    let lines: Vec<&str> = text.lines().collect();
    let mut res = HashResult::default();
    let size_re = Regex::new(r"^Size:\s+(\d+)$").unwrap();
    let files_re = Regex::new(r"^Files:\s+(\d+)$").unwrap();
    for ln in &lines {
        let s = ln.trim();
        if let Some(m) = size_re.captures(s) {
            res.size = m[1].parse().ok();
            continue;
        }
        if let Some(m) = files_re.captures(s) {
            res.files = m[1].parse().ok();
            continue;
        }
        if let Some(m) = HASH_RE.captures(s) {
            let (name, kind, val) = (m[1].to_string(), m[2].to_string(), m[3].to_string());
            let idx = match res.rows.iter().position(|r| r.name == name) {
                Some(i) => i,
                None => {
                    res.rows.push(HashRow { name: name.clone(), ..Default::default() });
                    res.rows.len() - 1
                }
            };
            if kind.ends_with("names") {
                res.rows[idx].names = Some(val);
            } else {
                res.rows[idx].data = Some(val);
            }
        }
    }
    let hexrow = Regex::new(r"^[0-9A-Fa-f]{8}").unwrap();
    for i in 1..lines.len() {
        if RULE_RE.is_match(lines[i]) && lines[i - 1].contains("Name") && lines[i - 1].contains("Size") {
            let rule = lines[i];
            let mut spans: Vec<(usize, usize)> = Vec::new();
            let bytes = rule.as_bytes();
            let mut k = 0;
            while k < bytes.len() {
                if bytes[k] == b'-' {
                    let st = k;
                    while k < bytes.len() && bytes[k] == b'-' {
                        k += 1;
                    }
                    spans.push((st, k));
                } else {
                    k += 1;
                }
            }
            if spans.len() < 2 {
                break;
            }
            let head = lines[i - 1];
            let slice = |s: &str, a: usize, b: usize| -> String {
                let a = a.min(s.len());
                let b = b.min(s.len());
                s.get(a..b).unwrap_or("").trim().to_string()
            };
            let heads: Vec<String> = spans[..spans.len() - 1].iter().map(|(a, b)| slice(head, *a, *b)).collect();
            let mut j = i + 1;
            while j < lines.len() && !lines[j].trim().is_empty() && !RULE_RE.is_match(lines[j]) {
                let row = lines[j];
                if !hexrow.is_match(row) {
                    j += 1;
                    continue;
                }
                let mut size = None;
                let mut hashes = Vec::new();
                for (idx, (a, b)) in spans[..spans.len() - 1].iter().enumerate() {
                    let v = slice(row, *a, *b);
                    if heads[idx] == "Size" {
                        size = v.parse().ok();
                    } else {
                        hashes.push((heads[idx].clone(), v));
                    }
                }
                let name = row.get(spans[spans.len() - 1].0..).unwrap_or("").trim().to_string();
                res.items.push(HashItem { name, size, hashes });
                j += 1;
            }
            break;
        }
    }
    res
}

// ---------------------------------------------------------------------------------------------
// creating archives
// ---------------------------------------------------------------------------------------------
#[derive(Clone, Debug)]
pub struct CreateOpts {
    pub sources: Vec<PathBuf>,
    pub out: String,
    pub fmt: String,
    pub level: u32,
    pub method: String,
    pub dict: String,
    pub word: u32,
    pub solid: String,
    pub threads: u32,
    pub memuse: u32,
    pub volume: String,
    pub params: String,
    pub update: String, // add | update | fresh | sync
    pub pathmode: String, // rel | full | abs
    pub sfx: bool,
    pub sdel: bool,
    pub shared: bool,
    pub enc_method: String, // AES256 | ZipCrypto
    pub password: String,
    pub enc_names: bool,
    pub if_exists: String, // "" | add | overwrite
}
impl Default for CreateOpts {
    fn default() -> Self {
        CreateOpts {
            sources: vec![],
            out: String::new(),
            fmt: "7z".into(),
            level: 5,
            method: String::new(),
            dict: String::new(),
            word: 0,
            solid: String::new(),
            threads: 0,
            memuse: 0,
            volume: String::new(),
            params: String::new(),
            update: "add".into(),
            pathmode: "rel".into(),
            sfx: false,
            sdel: false,
            shared: false,
            enc_method: "AES256".into(),
            password: String::new(),
            enc_names: false,
            if_exists: String::new(),
        }
    }
}

fn comp_switches(t: &str, fmt_key: &str, o: &CreateOpts) -> Vec<String> {
    let mut a = Vec::new();
    let allowed = methods_for(if t == "7z" || t == "zip" { t } else { fmt_key });
    let allowed: &[&str] = if allowed.is_empty() { methods_for(t) } else { allowed };
    let m = o.method.as_str();
    let eff: &str = if allowed.contains(&m) {
        m
    } else {
        match t {
            "7z" | "xz" => "LZMA2",
            "zip" | "gzip" => "Deflate",
            "bzip2" => "BZip2",
            _ => "",
        }
    };
    if allowed.contains(&m) && t == "7z" {
        a.push(format!("-m0={m}"));
    } else if allowed.contains(&m) && t == "zip" {
        a.push(format!("-mm={m}"));
    }
    if t != "tar" && t != "wim" {
        a.push(format!("-mx={}", if LEVELS.contains(&o.level) { o.level } else { 5 }));
    }
    let d = o.dict.as_str();
    let size_ok = !d.is_empty() && d.len() <= 6 && d[..d.len() - 1].chars().all(|c| c.is_ascii_digit()) && d.chars().last().map(|c| "kKmMgG".contains(c) || c.is_ascii_digit()).unwrap_or(false);
    if size_ok {
        if eff == "PPMd" {
            a.push(format!("-mmem={d}"));
        } else if ["LZMA2", "LZMA", "BZip2"].contains(&eff) {
            a.push(format!("-md={d}"));
        }
    }
    if o.word > 0 {
        match eff {
            "PPMd" => a.push(format!("-mo={}", o.word.clamp(2, 32))),
            "LZMA2" | "LZMA" => a.push(format!("-mfb={}", o.word.clamp(5, 273))),
            "Deflate" | "Deflate64" => a.push(format!("-mfb={}", o.word.clamp(3, 258))),
            _ => {}
        }
    }
    if t != "tar" && t != "wim" {
        if o.threads > 0 {
            a.push(format!("-mmt={}", o.threads.min(256)));
        }
        if (1..=100).contains(&o.memuse) {
            a.push(format!("-mmemuse=p{}", o.memuse));
        }
    }
    if t == "7z" && !o.solid.is_empty() && SOLID_OPTIONS.contains(&o.solid.as_str()) {
        a.push(format!("-ms={}", o.solid));
    }
    a
}

pub const ALL_EXTS: &[&str] = &[".tar.gz", ".tar.bz2", ".tar.xz", ".7z", ".zip", ".tar", ".gz", ".bz2", ".xz", ".wim", ".exe", ".sfx"];

pub fn strip_known_ext(name: &str) -> String {
    let low = name.to_lowercase();
    for e in ALL_EXTS {
        if low.ends_with(e) {
            return name[..name.len() - e.len()].to_string();
        }
    }
    name.to_string()
}

pub fn out_ext(fmt: &str, sfx: bool) -> &'static str {
    if fmt == "7z" && sfx {
        return if IS_WIN { ".exe" } else { ".sfx" };
    }
    fmt_spec(fmt).map(|f| f.ext).unwrap_or(".7z")
}

pub enum Prepared {
    Exists { noadd: bool, out: PathBuf },
    Ready(Box<Plan>),
}
pub struct Plan {
    pub args: Vec<String>,
    pub opts: JobOpts,
    pub out: PathBuf,
    pub open: PathBuf,
}

fn abs_path(p: &str) -> PathBuf {
    let pb = PathBuf::from(p);
    if pb.is_absolute() { pb } else { std::env::current_dir().map(|d| d.join(&pb)).unwrap_or(pb) }
}

pub fn prepare_create(_exe: &Path, o: &CreateOpts) -> Result<Prepared, Err> {
    let sources: Vec<&PathBuf> = o.sources.iter().filter(|s| !s.as_os_str().is_empty()).collect();
    if sources.is_empty() {
        return Err(err("E_NO_SOURCES"));
    }
    for s in &sources {
        if !s.exists() {
            return Err(err_arg("E_NOT_FOUND", s.to_string_lossy()));
        }
    }
    let spec = fmt_spec(&o.fmt).ok_or_else(|| err("E_BAD_FORMAT"))?;
    let t = spec.t;
    if spec.single && (sources.len() != 1 || !sources[0].is_file()) {
        return Err(err_arg("E_SINGLE_FILE", o.fmt.clone()));
    }
    let out_s = o.out.trim();
    if out_s.is_empty() {
        return Err(err("E_OUT_EMPTY"));
    }
    let ext = out_ext(&o.fmt, o.sfx);
    let mut out = out_s.to_string();
    if !out.to_lowercase().ends_with(ext) {
        let stripped = strip_known_ext(&out);
        out = if stripped.len() != out.len() { stripped } else { out };
        out.push_str(ext);
    }
    let out = abs_path(&out);
    if !out.parent().map(|p| p.is_dir()).unwrap_or(false) {
        return Err(err_arg("E_OUT_DIR", out.parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default()));
    }
    let pw = o.password.as_str();
    if !pw.is_empty() && o.fmt != "7z" && o.fmt != "zip" {
        return Err(err("E_NO_ENC"));
    }
    if o.sdel && spec.tar {
        return Err(err("E_BAD_REQUEST"));
    }
    let updatable = UPDATABLE.contains(&t) && !spec.tar;
    let upd_sw = match o.update.as_str() {
        "update" => Some("-up1q1r2x2y2z1w2"),
        "fresh" => Some("-up1q1r0x2y2z1w2"),
        "sync" => Some("-up1q0r2x2y2z1w2"),
        _ => None,
    };
    if out.exists() {
        let mut action = o.if_exists.as_str();
        if upd_sw.is_some() && updatable {
            action = "add";
        }
        if !updatable {
            if action != "overwrite" {
                return Ok(Prepared::Exists { noadd: true, out });
            }
        } else if action != "add" && action != "overwrite" {
            return Ok(Prepared::Exists { noadd: false, out });
        }
        if action == "overwrite" {
            if let Err(e) = std::fs::remove_file(&out) {
                return Err(err_arg("E_RM_FAIL", e.to_string()));
            }
        }
    }
    let mut extra: Vec<String> = Vec::new();
    match o.pathmode.as_str() {
        "full" => extra.push("-spf".into()),
        "abs" => extra.push("-spf2".into()),
        _ => {}
    }
    let vol: String = o.volume.trim().replace(' ', "");
    if !vol.is_empty() {
        static VOL_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{1,12}[bBkKmMgG]?(,\d{1,12}[bBkKmMgG]?)*$").unwrap());
        if !VOL_RE.is_match(&vol) || !["7z", "zip", "tar"].contains(&t) || spec.tar {
            return Err(err("E_BAD_REQUEST"));
        }
    }
    let mut user: Vec<String> = Vec::new();
    for tok in split_params(&o.params) {
        if !tok.starts_with('-') {
            return Err(err_arg("E_PARAMS", tok));
        }
        user.push(tok);
    }
    let src_s: Vec<String> = sources.iter().map(|s| s.to_string_lossy().to_string()).collect();
    let out_s = out.to_string_lossy().to_string();

    if spec.tar {
        let base = out.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let inner = Regex::new(r"(?i)\.(gz|bz2|xz)$").unwrap().replace(&base, "").to_string();
        let mut c1: Vec<String> = ["a", "-ttar", "-so", "-bsp2", "-bso2", "-bse2", "-bb1"].iter().map(|s| s.to_string()).collect();
        c1.extend(extra);
        c1.push("--".into());
        c1.push("pipe.tar".into());
        c1.extend(src_s);
        let mut c2: Vec<String> = vec!["a".into(), format!("-t{t}"), format!("-si{inner}"), "-bso0".into(), "-bsp0".into(), "-bse2".into()];
        c2.extend(comp_switches(t, &o.fmt, o));
        c2.extend(user);
        c2.push("--".into());
        c2.push(out_s);
        return Ok(Prepared::Ready(Box::new(Plan {
            args: c1,
            opts: JobOpts { out: Some(out.clone()), parse_sum: true, pipe: Some(c2), ..Default::default() },
            open: out.clone(),
            out,
        })));
    }
    let mut cmd: Vec<String> = vec!["a".into(), format!("-t{t}"), "-bsp1".into(), "-bb1".into()];
    cmd.extend(comp_switches(t, &o.fmt, o));
    cmd.extend(extra);
    if updatable {
        if let Some(sw) = upd_sw {
            cmd.push(sw.into());
        }
    }
    if !vol.is_empty() {
        cmd.push(format!("-v{vol}"));
    }
    if !pw.is_empty() {
        cmd.push(format!("-p{pw}"));
        if t == "zip" {
            cmd.push(format!("-mem={}", if o.enc_method == "ZipCrypto" { "ZipCrypto" } else { "AES256" }));
        }
        if t == "7z" && o.enc_names {
            cmd.push("-mhe=on".into());
        }
    }
    if t == "7z" && o.sfx {
        cmd.push("-sfx".into());
    }
    if o.sdel {
        cmd.push("-sdel".into());
    }
    if o.shared && IS_WIN {
        cmd.push("-ssw".into());
    }
    cmd.extend(user);
    cmd.push("--".into());
    cmd.push(out_s.clone());
    cmd.extend(src_s);
    let open = if vol.is_empty() { out.clone() } else { PathBuf::from(format!("{out_s}.001")) };
    Ok(Prepared::Ready(Box::new(Plan {
        args: cmd,
        opts: JobOpts { out: Some(out.clone()), parse_sum: true, ..Default::default() },
        out,
        open,
    })))
}

// ---------------------------------------------------------------------------------------------
// system info + benchmark parsing
// ---------------------------------------------------------------------------------------------
#[derive(Default, Clone, Debug)]
pub struct SysInfo {
    pub cpu: String,
    pub threads: u32,
    pub ram_mb: Option<u64>,
    pub os: String,
    pub arch: String,
}

pub fn sysinfo() -> SysInfo {
    let mut s = SysInfo { threads: std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(1), ..Default::default() };
    s.arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "arm" => "arm",
        "x86" => "x86",
        o => o,
    }
    .to_string();
    #[cfg(target_os = "linux")]
    {
        if let Ok(t) = std::fs::read_to_string("/proc/cpuinfo") {
            s.cpu = t.lines().find(|l| l.starts_with("model name")).and_then(|l| l.split_once(':')).map(|x| x.1.trim().to_string()).unwrap_or_default();
        }
        if let Ok(t) = std::fs::read_to_string("/proc/meminfo") {
            s.ram_mb = t.lines().find(|l| l.starts_with("MemTotal")).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse::<u64>().ok()).map(|kb| kb / 1024);
        }
        if let Ok(t) = std::fs::read_to_string("/proc/sys/kernel/osrelease") {
            s.os = format!("Linux {}", t.trim());
        }
    }
    #[cfg(target_os = "macos")]
    {
        let sc = |k: &str| Command::new("sysctl").args(["-n", k]).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
        s.cpu = sc("machdep.cpu.brand_string");
        s.ram_mb = sc("hw.memsize").parse::<u64>().ok().map(|b| b / 1048576);
        s.os = format!("macOS {}", sc("kern.osrelease"));
    }
    #[cfg(windows)]
    {
        s.cpu = std::env::var("PROCESSOR_IDENTIFIER").unwrap_or_default();
        s.os = "Windows".into();
        #[repr(C)]
        struct MemStatus {
            len: u32,
            load: u32,
            total_phys: u64,
            avail_phys: u64,
            total_page: u64,
            avail_page: u64,
            total_virt: u64,
            avail_virt: u64,
            avail_ext: u64,
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GlobalMemoryStatusEx(m: *mut MemStatus) -> i32;
        }
        let mut m = MemStatus { len: std::mem::size_of::<MemStatus>() as u32, load: 0, total_phys: 0, avail_phys: 0, total_page: 0, avail_page: 0, total_virt: 0, avail_virt: 0, avail_ext: 0 };
        if unsafe { GlobalMemoryStatusEx(&mut m) } != 0 {
            s.ram_mb = Some(m.total_phys / 1048576);
        }
    }
    s
}

#[derive(Default, Clone, Debug)]
pub struct BenchRow {
    pub dict: u32,
    pub c: [u64; 4],
    pub d: [u64; 4],
}
#[derive(Default, Clone, Debug)]
pub struct BenchData {
    pub rows: Vec<BenchRow>,
    pub avr: Option<([u64; 4], [u64; 4])>,
    pub tot: Option<[u64; 3]>,
    pub freq: Vec<String>,
    pub ram_use: Option<u64>,
    pub ram_total: Option<u64>,
}

pub fn parse_bench(lines: &[String]) -> BenchData {
    static ROW: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d+):\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s+\|\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s*$").unwrap());
    static AVR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^Avr:\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s+\|\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)").unwrap());
    static TOT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^Tot:\s+(\d+)\s+(\d+)\s+(\d+)").unwrap());
    static RAMS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"RAM size:\s+(\d+) MB").unwrap());
    static RAMU: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"RAM usage:\s+(\d+) MB").unwrap());
    let mut d = BenchData::default();
    let n = |m: &regex::Captures, i: usize| m[i].parse::<u64>().unwrap_or(0);
    for ln in lines {
        if let Some(m) = RAMS.captures(ln) {
            d.ram_total = m[1].parse().ok();
        } else if let Some(m) = RAMU.captures(ln) {
            d.ram_use = m[1].parse().ok();
        } else if ln.contains("CPU Freq") {
            d.freq.push(ln.trim().to_string());
        } else if let Some(m) = ROW.captures(ln) {
            d.rows.push(BenchRow { dict: m[1].parse().unwrap_or(0), c: [n(&m, 2), n(&m, 3), n(&m, 4), n(&m, 5)], d: [n(&m, 6), n(&m, 7), n(&m, 8), n(&m, 9)] });
        } else if let Some(m) = AVR.captures(ln) {
            d.avr = Some(([n(&m, 1), n(&m, 2), n(&m, 3), n(&m, 4)], [n(&m, 5), n(&m, 6), n(&m, 7), n(&m, 8)]));
        } else if let Some(m) = TOT.captures(ln) {
            d.tot = Some([n(&m, 1), n(&m, 2), n(&m, 3)]);
        }
    }
    d
}

// ---------------------------------------------------------------------------------------------
// formatting helpers shared by the GUI
// ---------------------------------------------------------------------------------------------
pub fn fmt_size(n: u64) -> String {
    let mut v = n as f64;
    for u in ["B", "KB", "MB", "GB", "TB"] {
        if v < 1024.0 || u == "TB" {
            return if u == "B" { format!("{} B", n) } else { format!("{v:.1} {u}") };
        }
        v /= 1024.0;
    }
    unreachable!()
}
pub fn fmt_mb(n: f64) -> String {
    if n >= 1048576.0 { format!("{} MB", (n / 1048576.0).round() as u64) } else { format!("{} KB", (n / 1024.0).round() as u64) }
}
pub fn fmt_hms(s: f64) -> String {
    let s = s.round() as u64;
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}
pub fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn params_split() {
        assert_eq!(split_params("-mqs=on  \"-x!a b\" '-y'"), vec!["-mqs=on", "-x!a b", "-y"]);
    }
    #[test]
    fn progress_lines() {
        let m = PROG_RE.captures("  62% 6      - dir/file.txt").unwrap();
        assert_eq!(&m[1], "62");
        assert_eq!(&m[2], "6");
        assert_eq!(&m[4], "dir/file.txt");
        assert!(PROG_RE.captures("0% path/x").is_some());
    }

    fn wait(j: &Job) -> Snapshot {
        for _ in 0..600 {
            if j.is_done() { break; }
            std::thread::sleep(Duration::from_millis(50));
        }
        j.snapshot()
    }
    #[test]
    fn real_7z_roundtrip() {
        let Some(sz) = find_7z() else { eprintln!("no 7z, skipped"); return; };
        eprintln!("using {:?}: {}", sz.exe, sz.banner);
        let tmp = std::env::temp_dir().join("u7z_test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("src/sub")).unwrap();
        std::fs::write(tmp.join("src/a.txt"), "hello hello hello\n".repeat(20000)).unwrap();
        std::fs::write(tmp.join("src/sub/b.txt"), "world\n".repeat(5000)).unwrap();
        for (fmt, extra) in [("7z", 0), ("zip", 0), ("tar", 0), ("tar.gz", 0), ("tar.xz", 0), ("wim", 0), ("7z", 1), ("zip", 2)] {
            let mut o = CreateOpts { sources: vec![tmp.join("src")], out: tmp.join(format!("o{extra}")).to_string_lossy().to_string(), fmt: fmt.into(), ..Default::default() };
            if extra == 1 { o.method = "PPMd".into(); o.dict = "16m".into(); o.word = 8; o.password = "pw".into(); o.enc_names = true; o.solid = "off".into(); o.threads = 1; }
            if extra == 2 { o.method = "BZip2".into(); o.password = "pw".into(); o.enc_method = "ZipCrypto".into(); }
            let Prepared::Ready(plan) = prepare_create(&sz.exe, &o).map_err(|e| e.code).unwrap() else { panic!("exists") };
            let j = Job::start(&sz.exe, plan.args.clone(), plan.opts.clone());
            let s = wait(&j);
            eprintln!("{fmt}/{extra}: rc={:?} pct={:?} files={:?}/{:?} total={:?} comp={:?} -> {:?}", s.rc, s.pct, s.files, s.files_total, s.total, s.comp, plan.out);
            assert_eq!(s.rc, Some(0), "{}", j.text());
            let pw = if extra > 0 { "pw" } else { "" };
            let a = list_archive(&sz, &plan.open, pw).map_err(|_| "list failed").unwrap();
            let nfiles = a.entries.iter().filter(|e| !e.dir).count();
            eprintln!("   listed {} entries, {} files, props={}", a.entries.len(), nfiles, a.props.len());
            assert!(nfiles >= 1);
        }
        // extract + hash
        let dest = tmp.join("ex");
        let ex = ExtractOpts { archive: tmp.join("o0.7z"), dest: dest.clone(), password: String::new(), keep_paths: true, mode: "aoa".into(), items: vec![] };
        let j = Job::start(&sz.exe, extract_args(&ex), JobOpts::default());
        assert_eq!(wait(&j).rc, Some(0));
        assert!(dest.join("src/sub/b.txt").is_file());
        let j = Job::start(&sz.exe, hash_files_args(&[tmp.join("src/a.txt")]), JobOpts { parse_sum: true, ..Default::default() });
        assert_eq!(wait(&j).rc, Some(0));
        let h = parse_hash(&j.text());
        eprintln!("hash rows: {:?} size {:?}", h.rows.iter().map(|r| r.name.clone()).collect::<Vec<_>>(), h.size);
        assert!(!h.rows.is_empty());
        // bad password
        assert!(matches!(list_archive(&sz, &tmp.join("o1.7z"), ""), Err(OpenError::NeedPassword { .. })));
        // pause/cancel
        let mut x: u64 = 88172645463325252;
        let big: Vec<u8> = (0..24_000_000).map(|_| { x ^= x << 13; x ^= x >> 7; x ^= x << 17; (x >> 32) as u8 }).collect();
        std::fs::create_dir_all(tmp.join("big")).unwrap();
        std::fs::write(tmp.join("big/r.bin"), big).unwrap();
        let mut o = CreateOpts { sources: vec![tmp.join("big")], out: tmp.join("slow").to_string_lossy().to_string(), level: 9, ..Default::default() };
        o.update = "add".into();
        let Prepared::Ready(plan) = prepare_create(&sz.exe, &o).map_err(|e| e.code).unwrap() else { panic!() };
        let j = Job::start(&sz.exe, plan.args, plan.opts);
        std::thread::sleep(Duration::from_millis(300));
        j.set_paused(true);
        assert!(j.snapshot().paused);
        std::thread::sleep(Duration::from_millis(300));
        let e1 = j.snapshot().el; std::thread::sleep(Duration::from_millis(500)); let e2 = j.snapshot().el;
        assert!((e2 - e1).abs() < 0.05, "elapsed must freeze while paused: {e1} {e2}");
        j.set_background(true);
        j.set_paused(false);
        j.cancel();
        let s = wait(&j);
        eprintln!("cancelled: rc={:?} cancelled={}", s.rc, s.cancelled);
        // benchmark via pty
        let j = Job::start(&sz.exe, bench_args(2, 20, 1), JobOpts { use_pty: true, ..Default::default() });
        let s = wait(&j);
        let b = parse_bench(&j.lines());
        eprintln!("bench rc={:?} rows={} avr={:?} tot={:?} ram={:?}/{:?}", s.rc, b.rows.len(), b.avr, b.tot, b.ram_use, b.ram_total);
        assert!(!b.rows.is_empty());
    }

    #[test]
    fn bench_parse() {
        let lines: Vec<String> = "RAM size:    8024 MB,  # CPU hardware threads:   2\nRAM usage:     24 MB,  # Benchmark threads:      2\n18:       3813    50   6848   3398  |      21513    50   3501   1740\n18:       3985    50   7127   3551  |      21584    50   3509   1745\nAvr:      3899    50   6988   3474  |      21548    50   3505   1743\nTot:              50   5246   2608".lines().map(|l| l.to_string()).collect();
        let d = parse_bench(&lines);
        assert_eq!(d.rows.len(), 2);
        assert_eq!(d.rows[0].c, [3813, 50, 6848, 3398]);
        assert_eq!(d.rows[1].d, [21584, 50, 3509, 1745]);
        assert_eq!(d.avr.unwrap().0[0], 3899);
        assert_eq!(d.tot.unwrap()[2], 2608);
        assert_eq!(d.ram_use, Some(24));
    }
}
