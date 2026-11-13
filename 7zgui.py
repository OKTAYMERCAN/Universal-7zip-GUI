#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
7Zip GUI - a dependency-free GUI for the 7-Zip console program (Linux, Windows, macOS).

Needs no extra packages: only Python 3 (standard library) and a web browser.
The program starts a local server (127.0.0.1) and opens the interface in your browser.

INSTALL
  Linux  : Extract the "Linux x86-64" archive from 7-zip.org into an empty folder
           (it contains 7zz), put 7zgui.py in the same folder ->  python3 7zgui.py
  Windows: Install 7-Zip (or take 7za.exe / 7zr.exe from the 7-Zip "Extra" package).
           Put 7zgui.py next to 7z.exe, or have 7z.exe in PATH
           ->  py 7zgui.py   (rename the file to 7zgui.pyw to avoid the console window)
  macOS  : Put 7zz (from 7-zip.org or "brew install sevenzip") in the same folder or in PATH.

UPDATE
  Extract the new 7-Zip release into the same folder and overwrite the old files.
  7zgui.py does not change: on every start it looks for 7-Zip in its own folder first,
  then in PATH and in the usual install folders.

FEATURES
  * Open / create / extract / test archives; 7z, zip, tar, gzip, bzip2, xz, wim and tar.gz/.bz2/.xz
  * "Add to Archive" window: compression level/method, dictionary, word and solid block size,
    CPU threads, memory limit, split to volumes, extra parameters, update/path mode,
    SFX, encryption (AES-256 / ZipCrypto)
  * Windows-style progress window (Background / Pause / Cancel), archive Properties,
    Checksum (CRC32, SHA-256 ...), Benchmark
  * Themes: System / Light / Dark / OLED (pure black); languages: EN TR DE FR ES RU ZH

USAGE
  python3 7zgui.py                    -> empty window
  python3 7zgui.py archive.7z         -> open the archive directly
  python3 7zgui.py --add file folder  -> open "Add to Archive" with these sources
  Options: --no-browser (only print the address)  --port N  --no-app
"""
import argparse
import collections
import json
import mimetypes
import os
import platform
import re
import secrets
import shlex
import signal
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import webbrowser
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, quote, urlparse

HERE = os.path.dirname(os.path.realpath(__file__))
IS_WIN = os.name == "nt"
IS_MAC = sys.platform == "darwin"
NO_WINDOW = 0x08000000 if IS_WIN else 0  # CREATE_NO_WINDOW
_BASE_NAMES = ("7zz", "7zzs", "7z", "7za", "7zr")
CANDIDATES = tuple(n + ".exe" for n in _BASE_NAMES) if IS_WIN else _BASE_NAMES
LEVELS = (0, 1, 3, 5, 7, 9)

# arayuzdeki biçim adi -> 7-Zip turu, uzanti, tek dosya mi, tar+sikistirma mi
FMT = {
    "7z": {"t": "7z", "ext": ".7z"},
    "zip": {"t": "zip", "ext": ".zip"},
    "tar": {"t": "tar", "ext": ".tar"},
    "gzip": {"t": "gzip", "ext": ".gz", "single": True},
    "bzip2": {"t": "bzip2", "ext": ".bz2", "single": True},
    "xz": {"t": "xz", "ext": ".xz", "single": True},
    "wim": {"t": "wim", "ext": ".wim"},
    "tar.gz": {"t": "gzip", "ext": ".tar.gz", "tar": True},
    "tar.bz2": {"t": "bzip2", "ext": ".tar.bz2", "tar": True},
    "tar.xz": {"t": "xz", "ext": ".tar.xz", "tar": True},
}
METHODS = {
    "7z": ("LZMA2", "LZMA", "PPMd", "BZip2", "Deflate", "Deflate64", "Copy"),
    "zip": ("Deflate", "Deflate64", "BZip2", "LZMA", "PPMd", "Copy"),
    "gzip": ("Deflate",), "bzip2": ("BZip2",), "xz": ("LZMA2",),
}
DEF_METHOD = {"7z": "LZMA2", "zip": "Deflate", "gzip": "Deflate", "bzip2": "BZip2", "xz": "LZMA2"}
UPDATABLE = ("7z", "zip", "tar", "wim")
UPDATE_SW = {"update": "-up1q1r2x2y2z1w2", "fresh": "-up1q1r0x2y2z1w2", "sync": "-up1q0r2x2y2z1w2"}
SIZE_RE = re.compile(r"^\d{1,5}[kKmMgG]?$")
VOL_RE = re.compile(r"^\d{1,12}[bBkKmMgG]?(,\d{1,12}[bBkKmMgG]?)*$")
SOLID_RE = re.compile(r"^(on|off|\d{1,3}[mMgG])$")
OVERWRITE = {"aoa", "aos", "aou", "aot"}
IDLE_LIMIT = 180  # saniye: tarayicidan sinyal gelmezse sunucu kapanir

# "  62% 6      - dizin/dosya"  ->  yuzde, islenen dosya sayisi, isaret, yol
PROG_RE = re.compile(r"^\s*(\d{1,3})%(?:\s+(\d+))?(?:\s+(?:([-+UDRT.=!])\s+)?(.+?))?\s*$")
PART_RE = re.compile(r"^\s*(\d{1,3})%(?:\s+(\d+)\s)?")
FILE_RE = re.compile(r"^([-+UDRT.=!])\s+(.+)$")
SUM_RE = re.compile(r"(?:^|:)\s*(?:\d+ folders?,\s*)?(\d+) files?,\s*(\d+) bytes")
HASH_RE = re.compile(r"^(\S+)\s+for (data and names|data):\s*(\S+)\s*$")
RULE_RE = re.compile(r"^-+(?: +-+)+\s*$")
CTRL_RE = re.compile(r"[\x00-\x08\x0b-\x1f\x7f]")

STATE = {
    "exe": None,
    "version": "",
    "banner": "",
    "token": "",
    "port": 0,
    "last_ping": time.time(),
    "server": None,
    "tmp": None,
}
JOBS = {}
JOBS_LOCK = threading.Lock()


# --------------------------------------------------------------------------
# 7-Zip'i bulma
# --------------------------------------------------------------------------
def utf8_env():
    env = os.environ.copy()
    loc = env.get("LC_ALL") or env.get("LC_CTYPE") or env.get("LANG") or ""
    if "utf" not in loc.lower():
        env["LC_ALL"] = "C.UTF-8"
    return env


def popen_kw():
    return {"creationflags": NO_WINDOW} if IS_WIN else {}


def probe(path):
    """Calistirilabiliyor ve 7-Zip mi? Basarili ise ilk satiri dondurur."""
    try:
        r = subprocess.run([path], stdin=subprocess.DEVNULL, capture_output=True,
                           timeout=10, env=utf8_env(), **popen_kw())
    except (OSError, subprocess.SubprocessError):
        return None
    out = (r.stdout + r.stderr).decode("utf-8", "replace")
    for line in out.splitlines():
        if "7-Zip" in line:
            return line.strip()
    return None


def known_dirs():
    if IS_WIN:
        dirs = [os.environ.get("ProgramFiles"), os.environ.get("ProgramFiles(x86)"),
                os.environ.get("ProgramW6432")]
        return [os.path.join(d, "7-Zip") for d in dirs if d]
    if IS_MAC:
        return ["/opt/homebrew/bin", "/usr/local/bin", "/Applications/7-Zip"]
    return ["/usr/local/bin", "/usr/lib/7zip", "/usr/libexec/7zip"]


def find_7z():
    """Once bu script'in klasorune, sonra PATH'e, sonra bilinen kurulum klasorlerine bakar."""
    places = [HERE]
    for name in CANDIDATES:
        p = None
        for d in places:
            c = os.path.join(d, name)
            if os.path.isfile(c):
                p = c
                break
        if p and not IS_WIN and not os.access(p, os.X_OK):
            try:
                os.chmod(p, os.stat(p).st_mode | 0o111)
            except OSError:
                pass
        if p:
            banner = probe(p)
            if banner:
                return p, banner
    for name in CANDIDATES:
        p = shutil.which(name)
        if p:
            banner = probe(p)
            if banner:
                return p, banner
    for d in known_dirs():
        for name in CANDIDATES:
            c = os.path.join(d, name)
            if os.path.isfile(c):
                banner = probe(c)
                if banner:
                    return c, banner
    return None, ""


# --------------------------------------------------------------------------
# Arka plan isleri
# --------------------------------------------------------------------------
def win_proc_call(proc, fn_name):
    import ctypes
    fn = getattr(ctypes.windll.ntdll, fn_name)
    return fn(ctypes.c_void_p(int(proc._handle))) == 0  # noqa: SLF001


def win_priority(proc, low):
    import ctypes
    return bool(ctypes.windll.kernel32.SetPriorityClass(
        ctypes.c_void_p(int(proc._handle)), 0x4000 if low else 0x20))  # noqa: SLF001


def with_charset(cmd):
    """Windows'ta 7z.exe konsol ciktisini UTF-8 yapar (Turkce/Unicode dosya adlari)."""
    return cmd[:1] + ["-sccUTF-8"] + cmd[1:] if IS_WIN and cmd else cmd


class Job:
    def __init__(self, cmd, quiet=False, extra=None, total=None, packed=None,
                 files_total=None, out=None, parse_sum=False, use_pty=False, pipe=None):
        self.cmd = cmd
        self.pipe = pipe  # ikinci komut: cmd | pipe
        self.quiet = quiet
        self.extra = extra or {}
        self.total = total
        self.packed = packed
        self.files_total = files_total
        self.out = out
        self.parse_sum = parse_sum
        self.use_pty = use_pty and not IS_WIN
        self.proc = None
        self.procs = []
        self.cancelled = False
        self.raw = bytearray()
        self.lines = []
        self.pct = None
        self.files = None
        self.cur = ""
        self.rc = None
        self.start = time.monotonic()
        self.end_time = None
        self.paused = False
        self.pause_at = None
        self.paused_total = 0.0
        self.background = False
        self.samples = collections.deque()
        self.done = threading.Event()
        threading.Thread(target=self._run, daemon=True).start()

    # -- kontrol
    def alive(self):
        return any(p.poll() is None for p in self.procs)

    def cancel(self):
        self.cancelled = True
        if self.paused:
            self.set_paused(False)
        for p in self.procs:
            if p.poll() is None:
                try:
                    p.terminate()
                except OSError:
                    pass

    def set_paused(self, on):
        if not self.alive():
            return False
        try:
            if on and not self.paused:
                for p in self.procs:
                    if p.poll() is None:
                        if IS_WIN:
                            win_proc_call(p, "NtSuspendProcess")
                        else:
                            os.kill(p.pid, signal.SIGSTOP)
                self.paused, self.pause_at = True, time.monotonic()
            elif not on and self.paused:
                for p in self.procs:
                    if p.poll() is None:
                        if IS_WIN:
                            win_proc_call(p, "NtResumeProcess")
                        else:
                            os.kill(p.pid, signal.SIGCONT)
                self.paused_total += time.monotonic() - self.pause_at
                self.paused, self.pause_at = False, None
        except (OSError, AttributeError):
            return False
        return True

    def set_background(self, on):
        """Windows'taki 'Arka plan' dugmesi: islem onceligini dusurur."""
        if not self.alive():
            return False
        ok = True
        for p in self.procs:
            if p.poll() is not None:
                continue
            if IS_WIN:
                try:
                    ok = win_priority(p, on) and ok
                except (OSError, AttributeError):
                    ok = False
                continue
            try:
                tids = [int(t) for t in os.listdir(f"/proc/{p.pid}/task")]
            except (OSError, ValueError):
                tids = [p.pid]
            for tid in tids:
                try:
                    os.setpriority(os.PRIO_PROCESS, tid, 10 if on else 0)
                except OSError:
                    ok = False
        self.background = (ok and bool(on)) or (not ok and not on)
        return ok

    def elapsed(self):
        now = self.end_time if self.end_time is not None else time.monotonic()
        paused_now = (now - self.pause_at) if self.pause_at is not None else 0.0
        return max(0.0, now - self.start - self.paused_total - paused_now)

    def text(self):
        if self.quiet:
            return self.raw.decode("utf-8", "replace")
        return "\n".join(self.lines)

    # -- cikti ayristirma
    def _set_progress(self, pct, files):
        self.pct = min(int(pct), 100)
        if files:
            self.files = int(files)

    def _piece(self, b):
        line = CTRL_RE.sub("", b.decode("utf-8", "replace")).rstrip()
        text = line.strip()
        if not text:
            return
        m = PROG_RE.match(text)
        if m:
            self._set_progress(m.group(1), m.group(2))
            if m.group(4):
                self.cur = m.group(4)
            return
        if self.parse_sum:
            m = SUM_RE.search(text)
            if m:
                self.files_total, self.total = int(m.group(1)), int(m.group(2))
        m = FILE_RE.match(text)
        if m and not self.use_pty:
            self.cur = m.group(2)
            return
        if len(self.lines) < 5000:
            self.lines.append(line)

    def _partial(self, buf):
        m = PART_RE.match(buf.decode("utf-8", "replace"))
        if m:
            self._set_progress(m.group(1), m.group(2))

    def _finish(self, rc):
        self.rc = rc
        self.end_time = time.monotonic()
        self.done.set()

    def _drain(self, proc):
        """Boru hattinin ikinci surecinin (hata) ciktisini okur."""
        try:
            data = proc.stdout.read()
        except (OSError, ValueError):
            return
        for ln in data.replace(b"\r", b"\n").split(b"\n"):
            t = CTRL_RE.sub("", ln.decode("utf-8", "replace")).rstrip()
            if t.strip() and len(self.lines) < 5000:
                self.lines.append(t)

    def _run(self):
        fd, master, drain = None, None, None
        env, kw = utf8_env(), popen_kw()
        try:
            if self.pipe:
                p1 = subprocess.Popen(with_charset(self.cmd), stdin=subprocess.DEVNULL,
                                      stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env, **kw)
                try:
                    p2 = subprocess.Popen(with_charset(self.pipe), stdin=p1.stdout,
                                          stdout=subprocess.PIPE, stderr=subprocess.STDOUT, env=env, **kw)
                except OSError:
                    p1.kill()
                    raise
                p1.stdout.close()
                self.procs = [p1, p2]
                self.proc = p1
                fd = p1.stderr.fileno()
                drain = threading.Thread(target=self._drain, args=(p2,), daemon=True)
                drain.start()
            elif self.use_pty:
                import pty
                master, slave = pty.openpty()
                try:
                    self.proc = subprocess.Popen(
                        with_charset(self.cmd), stdin=subprocess.DEVNULL, stdout=slave, stderr=slave,
                        env=env, close_fds=True)
                finally:
                    os.close(slave)
                self.procs = [self.proc]
                fd = master
            else:
                self.proc = subprocess.Popen(
                    with_charset(self.cmd), stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                    stderr=subprocess.STDOUT, env=env, bufsize=0, **kw)
                self.procs = [self.proc]
                fd = self.proc.stdout.fileno()
        except (OSError, ImportError) as exc:
            self.raw.extend(str(exc).encode())
            self.lines.append(str(exc))
            if master is not None:
                os.close(master)
            self._finish(-1)
            return
        buf = b""
        while True:
            try:
                chunk = os.read(fd, 65536)
            except OSError:
                break
            if not chunk:
                break
            if self.quiet:
                self.raw.extend(chunk)
                continue
            buf += chunk
            parts = re.split(rb"[\r\n\x08]+", buf)
            buf = parts.pop()
            for p in parts:
                self._piece(p)
            self._partial(buf)
        if buf and not self.quiet:
            self._piece(buf)
        rc = 0
        for p in self.procs:
            rc = max(rc, p.wait())
        if drain:
            drain.join(5)
        if master is not None:
            try:
                os.close(master)
            except OSError:
                pass
        self._finish(rc)

    # -- arayuz icin anlik goruntu
    def snapshot(self, since=0):
        done = self.done.is_set()
        el = self.elapsed()
        pct = self.pct
        if done and self.rc in (0, 1) and not self.cancelled and self.total is not None:
            pct = 100
        total = self.total
        processed = total * pct / 100.0 if (total is not None and pct is not None) else None
        speed = None
        if processed is not None:
            s = self.samples
            if not s or s[-1][1] != processed:
                s.append((el, processed))
            while len(s) > 1 and el - s[1][0] >= 8:
                s.popleft()
            if el - s[0][0] >= 1.0:
                speed = max(0.0, (processed - s[0][1]) / (el - s[0][0]))
            elif el > 0.3:
                speed = processed / el
        rem = None
        if speed and total is not None and processed is not None:
            rem = max(0.0, (total - processed) / speed)
        comp = None
        if self.out:
            sizes = []
            for p in (self.out, self.out + ".tmp", self.out + ".001"):
                try:
                    sizes.append(os.path.getsize(p))
                except OSError:
                    pass
            comp = max(sizes) if sizes else None
        elif self.packed is not None and pct is not None:
            comp = self.packed * pct / 100.0
        ratio = (comp / processed * 100.0) if (comp is not None and processed) else None
        return {
            "pct": pct, "cur": self.cur, "files": self.files, "files_total": self.files_total,
            "el": el, "rem": rem, "total": total, "processed": processed, "speed": speed,
            "comp": comp, "ratio": ratio, "lines": self.lines[since:], "next": len(self.lines),
            "done": done, "rc": self.rc if done else None, "cancelled": self.cancelled,
            "paused": self.paused, "background": self.background,
        }


def start_job(cmd, **kw):
    job = Job(cmd, **kw)
    jid = secrets.token_hex(6)
    with JOBS_LOCK:
        JOBS[jid] = job
        if len(JOBS) > 30:  # eski bitmis isleri temizle
            for k in [k for k, j in JOBS.items() if j.done.is_set()][:-10]:
                JOBS.pop(k, None)
    return jid, job


def parse_slt(text, default_name=""):
    """`7z l -slt` ciktisinda '----------' satirindan sonraki kayitlari dondurur.
    gzip/bzip2/xz gibi ad saklamayan biçimlerde Path satiri yoktur: default_name kullanilir."""
    entries, cur, in_entries = [], {}, False
    for line in text.splitlines():
        if not in_entries:
            if line.strip() == "----------":
                in_entries = True
            continue
        if not line.strip():
            if cur and "Path" not in cur and default_name:
                cur["Path"] = default_name
            if "Path" in cur:
                entries.append(dict(cur))
            cur.clear()
            continue
        key, sep, val = line.partition(" = ")
        if sep:
            cur[key.strip()] = val
    if cur and "Path" not in cur and default_name:
        cur["Path"] = default_name
    if "Path" in cur:
        entries.append(dict(cur))
    return entries


def parse_props(text):
    """Arsiv basligi ('--' satirindan sonraki Path/Type/Method... blogu)."""
    props, started = [], False
    for line in text.splitlines():
        if line.strip() == "--":
            started = True
            continue
        if started:
            if not line.strip():
                break
            key, sep, val = line.partition(" = ")
            if sep:
                props.append([key.strip(), val])
    return props


def parse_hash(text):
    """`7z h/t -scrc*` ciktisini yapilandirir."""
    lines = text.splitlines()
    res = {"size": None, "files": None, "rows": [], "items": []}
    idx = {}
    for ln in lines:
        s = ln.strip()
        m = re.match(r"^Size:\s+(\d+)$", s)
        if m:
            res["size"] = int(m.group(1))
            continue
        m = re.match(r"^Files:\s+(\d+)$", s)
        if m:
            res["files"] = int(m.group(1))
            continue
        m = HASH_RE.match(s)
        if m:
            name, kind, val = m.groups()
            if name not in idx:
                idx[name] = {"h": name, "data": None, "names": None}
                res["rows"].append(idx[name])
            idx[name]["names" if kind.endswith("names") else "data"] = val
    for i in range(1, len(lines)):
        if RULE_RE.match(lines[i]) and "Name" in lines[i - 1] and "Size" in lines[i - 1]:
            spans = [(m.start(), m.end()) for m in re.finditer(r"-+", lines[i])]
            heads = [lines[i - 1][a:b].strip() for a, b in spans[:-1]]
            j = i + 1
            while j < len(lines) and lines[j].strip() and not RULE_RE.match(lines[j]):
                row = lines[j]
                if not re.match(r"^[0-9A-Fa-f]{8}", row):
                    j += 1
                    continue
                vals = {heads[k]: row[a:b].strip() for k, (a, b) in enumerate(spans[:-1])}
                size = to_int(vals.pop("Size", None))
                res["items"].append({"name": row[spans[-1][0]:].strip(), "size": size, "hashes": vals})
                j += 1
            break
    return res


def to_int(v):
    try:
        return int(v)
    except (TypeError, ValueError):
        return None


def pw_args(pw):
    return ["-p" + pw] if pw else []


# --------------------------------------------------------------------------
# API islemleri
# --------------------------------------------------------------------------
def fail(code, arg=None):
    """Hata kodu: metni arayuz kendi diline cevirir."""
    return {"error": code, "ecode": code, "arg": arg}


def base_name(p):
    return re.split(r"[\\/]" if IS_WIN else "/", p.rstrip("/\\" if IS_WIN else "/"))[-1]


def sysinfo():
    cpu, ram = "", None
    try:
        if sys.platform.startswith("linux"):
            with open("/proc/cpuinfo", encoding="utf-8", errors="replace") as fh:
                for ln in fh:
                    if ln.startswith("model name"):
                        cpu = ln.split(":", 1)[1].strip()
                        break
            with open("/proc/meminfo", encoding="utf-8", errors="replace") as fh:
                for ln in fh:
                    if ln.startswith("MemTotal"):
                        ram = int(ln.split()[1]) // 1024
                        break
        elif IS_WIN:
            import ctypes
            import winreg

            with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE,
                                r"HARDWARE\DESCRIPTION\System\CentralProcessor\0") as k:
                cpu = winreg.QueryValueEx(k, "ProcessorNameString")[0].strip()

            class MemStatus(ctypes.Structure):
                _fields_ = [("dwLength", ctypes.c_ulong), ("dwMemoryLoad", ctypes.c_ulong),
                            ("ullTotalPhys", ctypes.c_ulonglong), ("ullAvailPhys", ctypes.c_ulonglong),
                            ("ullTotalPageFile", ctypes.c_ulonglong), ("ullAvailPageFile", ctypes.c_ulonglong),
                            ("ullTotalVirtual", ctypes.c_ulonglong), ("ullAvailVirtual", ctypes.c_ulonglong),
                            ("ullAvailExtendedVirtual", ctypes.c_ulonglong)]

            ms = MemStatus()
            ms.dwLength = ctypes.sizeof(MemStatus)
            ctypes.windll.kernel32.GlobalMemoryStatusEx(ctypes.byref(ms))
            ram = ms.ullTotalPhys // 1048576
        elif IS_MAC:
            cpu = subprocess.run(["sysctl", "-n", "machdep.cpu.brand_string"], capture_output=True,
                                 text=True, timeout=5).stdout.strip()
            ram = int(subprocess.run(["sysctl", "-n", "hw.memsize"], capture_output=True,
                                     text=True, timeout=5).stdout.strip()) // 1048576
    except Exception:  # noqa: BLE001
        pass
    cpu = cpu or platform.processor() or platform.machine()
    arch = {"x86_64": "x64", "amd64": "x64", "aarch64": "arm64", "armv7l": "arm"}.get(
        platform.machine().lower(), platform.machine())
    return {"cpu": cpu, "threads": os.cpu_count() or 1, "ram": ram,
            "os": f"{platform.system()} {platform.release()}", "arch": arch}


def api_info(_q):
    d = {"exe": STATE["exe"], "version": STATE["version"], "banner": STATE["banner"],
         "home": os.path.expanduser("~"), "here": HERE, "sep": os.sep, "win": IS_WIN}
    d.update(sysinfo())
    return d


def list_drives():
    import ctypes
    mask = ctypes.windll.kernel32.GetLogicalDrives()
    return [f"{chr(65 + i)}:\\" for i in range(26) if mask & (1 << i)]


def api_ls(q):
    raw = q.get("path", [""])[0]
    show_hidden = q.get("hidden", ["0"])[0] == "1"
    if IS_WIN and raw == "@drives":
        return {"path": "", "parent": None,
                "entries": [{"n": d, "f": d, "d": True, "s": None, "m": 0} for d in list_drives()]}
    path = os.path.abspath(os.path.expanduser(raw or "~"))
    if os.path.isfile(path):
        path = os.path.dirname(path)
    entries = []
    try:
        with os.scandir(path) as it:
            for e in it:
                try:
                    st = e.stat()
                    hidden = e.name.startswith(".") or bool(getattr(st, "st_file_attributes", 0) & 2)
                    if hidden and not show_hidden:
                        continue
                    is_dir = e.is_dir()
                    entries.append({"n": e.name, "f": e.path, "d": is_dir,
                                    "s": None if is_dir else st.st_size, "m": int(st.st_mtime)})
                except OSError:
                    if show_hidden or not e.name.startswith("."):
                        entries.append({"n": e.name, "f": e.path, "d": False, "s": None, "m": 0})
    except OSError as exc:
        return fail("E_READ_DIR", exc.strerror or str(exc))
    entries.sort(key=lambda x: (not x["d"], x["n"].lower()))
    parent = os.path.dirname(path)
    if parent == path:
        parent = "@drives" if IS_WIN else None
    return {"path": path, "parent": parent, "entries": entries}


def api_mkdir(b):
    parent, name = b.get("path", ""), b.get("name", "").strip()
    if not name or "/" in name or os.sep in name or name in (".", ".."):
        return fail("E_BAD_DIRNAME")
    try:
        os.mkdir(os.path.join(parent, name))
    except OSError as exc:
        return fail("E_MKDIR", exc.strerror or str(exc))
    return {"ok": True, "path": os.path.join(parent, name)}


def api_open(b):
    path, pw = b.get("path", ""), b.get("password", "")
    if not STATE["exe"]:
        return fail("E_NO_7Z")
    if not os.path.isfile(path):
        return fail("E_NOT_FOUND", path)
    job = Job([STATE["exe"], "l", "-slt"] + pw_args(pw) + ["--", path], quiet=True)
    job.done.wait()
    text = job.text()
    if job.rc is not None and job.rc >= 2:
        low = text.lower()
        if "password" in low or "encrypted" in low:
            return {"needPassword": True, "wrong": bool(pw)}
        return fail("E_7Z", "\n".join(text.strip().splitlines()[-8:]) or "?")
    out = []
    dflt = os.path.splitext(os.path.basename(path))[0] or os.path.basename(path)
    for e in parse_slt(text, dflt):
        attr = e.get("Attributes", "")
        out.append({
            "p": e["Path"], "k": e["Path"].replace("\\", "/") if IS_WIN else e["Path"],
            "d": e.get("Folder") == "+" or attr.startswith("D"),
            "s": to_int(e.get("Size")), "c": to_int(e.get("Packed Size")),
            "m": e.get("Modified", "")[:19], "a": attr, "e": e.get("Encrypted") == "+",
        })
    try:
        size = os.path.getsize(path)
    except OSError:
        size = None
    return {"ok": True, "entries": out, "size": size, "props": parse_props(text)}


def need_exe():
    return None if STATE["exe"] else fail("E_NO_7Z")


def sizing(b):
    """Arayuzun bildirdigi toplam boyut/dosya sayisi (ilerleme bilgisi icin)."""
    return {"total": to_int(b.get("total")) or None, "packed": to_int(b.get("packed")),
            "files_total": to_int(b.get("files"))}


def item_args(b):
    out = []
    for it in b.get("items", []):
        p = str(it.get("p", ""))
        if p:
            out.append(p)
            if it.get("d"):
                out.append(p.rstrip("/\\") + ("\\" if IS_WIN else "/") + "*")
    return out


def api_extract(b):
    err = need_exe()
    if err:
        return err
    archive = b.get("archive", "")
    if not os.path.isfile(archive):
        return fail("E_ARC_NOT_FOUND")
    dest = (b.get("dest") or "").strip()
    if not dest:
        return fail("E_DEST_EMPTY")
    mode = b.get("mode", "aoa")
    if mode not in OVERWRITE:
        mode = "aoa"
    cmd = [STATE["exe"], "x" if b.get("keep", True) else "e", "-o" + dest, "-y", "-" + mode,
           "-bsp1", "-bb1"]
    cmd += pw_args(b.get("password", ""))
    cmd += ["--", archive]
    if b.get("scope") == "sel":
        cmd += item_args(b)
    jid, _ = start_job(cmd, **sizing(b))
    return {"job": jid}


def api_test(b):
    err = need_exe()
    if err:
        return err
    archive = b.get("archive", "")
    if not os.path.isfile(archive):
        return fail("E_ARC_NOT_FOUND")
    jid, _ = start_job([STATE["exe"], "t", "-bsp1", "-bb1"] + pw_args(b.get("password", ""))
                       + ["--", archive], **sizing(b))
    return {"job": jid}


def api_fetch(b):
    """Arsivden tek dosyayi gecici klasore cikarir (indirme icin)."""
    err = need_exe()
    if err:
        return err
    archive, path = b.get("archive", ""), b.get("path", "")
    if not os.path.isfile(archive) or not path:
        return fail("E_BAD_REQUEST")
    tmp = tempfile.mkdtemp(prefix="dl-", dir=STATE["tmp"])
    cmd = [STATE["exe"], "e", "-o" + tmp, "-y", "-aoa", "-bsp1"] + pw_args(b.get("password", ""))
    cmd += ["--", archive, path]
    name = base_name(path)
    jid, _ = start_job(cmd, extra={"file": os.path.join(tmp, name), "name": name})
    return {"job": jid}


def comp_switches(t, o):
    """Sikistirma yontemi/sozluk/kelime/solid/is parcacigi/bellek anahtarlari."""
    a = []
    allowed = METHODS.get(t, ())
    m = o.get("method")
    eff = m if m in allowed else DEF_METHOD.get(t)
    if m in allowed and t == "7z":
        a.append("-m0=" + m)
    elif m in allowed and t == "zip":
        a.append("-mm=" + m)
    if t not in ("tar", "wim"):
        lvl = to_int(o.get("level"))
        a.append(f"-mx={lvl if lvl in LEVELS else 5}")
    d = str(o.get("dict") or "")
    if d and SIZE_RE.match(d):
        if eff == "PPMd":
            a.append("-mmem=" + d)
        elif eff in ("LZMA2", "LZMA", "BZip2"):
            a.append("-md=" + d)
    w = to_int(o.get("word"))
    if w:
        if eff == "PPMd":
            a.append(f"-mo={min(max(w, 2), 32)}")
        elif eff in ("LZMA2", "LZMA"):
            a.append(f"-mfb={min(max(w, 5), 273)}")
        elif eff in ("Deflate", "Deflate64"):
            a.append(f"-mfb={min(max(w, 3), 258)}")
    if t not in ("tar", "wim"):
        th = to_int(o.get("threads"))
        if th and th > 0:
            a.append(f"-mmt={min(th, 256)}")
        mu = to_int(o.get("memuse"))
        if mu and 1 <= mu <= 100:
            a.append(f"-mmemuse=p{mu}")
    sol = str(o.get("solid") or "")
    if t == "7z" and SOLID_RE.match(sol):
        a.append("-ms=" + sol)
    return a


def api_create(b):
    err = need_exe()
    if err:
        return err
    sources = [s for s in b.get("sources", []) if isinstance(s, str) and s]
    if not sources:
        return fail("E_NO_SOURCES")
    for s in sources:
        if not os.path.exists(s):
            return fail("E_NOT_FOUND", s)
    fmt = b.get("fmt", "7z")
    spec = FMT.get(fmt)
    if not spec:
        return fail("E_BAD_FORMAT")
    t = spec["t"]
    if spec.get("single") and (len(sources) != 1 or not os.path.isfile(sources[0])):
        return fail("E_SINGLE_FILE", fmt)
    out = (b.get("out") or "").strip()
    if not out:
        return fail("E_OUT_EMPTY")
    ext = spec["ext"]
    if fmt == "7z" and b.get("sfx"):
        ext = ".exe" if IS_WIN else ".sfx"
    low = out.lower()
    if not low.endswith(ext):
        for e in sorted([f["ext"] for f in FMT.values()] + [".exe", ".sfx"], key=len, reverse=True):
            if low.endswith(e):
                out = out[: -len(e)] + ext
                break
        else:
            out += ext
    out = os.path.abspath(os.path.expanduser(out))
    if not os.path.isdir(os.path.dirname(out)):
        return fail("E_OUT_DIR", os.path.dirname(out))

    pw = str(b.get("password") or "")
    if pw and fmt not in ("7z", "zip"):
        return fail("E_NO_ENC")
    if b.get("sdel") and spec.get("tar"):
        return fail("E_BAD_REQUEST")
    upd = b.get("update", "add")
    updatable = t in UPDATABLE and not spec.get("tar")
    if os.path.exists(out):
        action = b.get("if_exists")
        if upd in UPDATE_SW and updatable:
            action = "add"
        if not updatable:
            if action != "overwrite":
                return {"exists": True, "noadd": True}
        elif action not in ("add", "overwrite"):
            return {"exists": True}
        if action == "overwrite":
            try:
                os.remove(out)
            except OSError as exc:
                return fail("E_RM_FAIL", exc.strerror or str(exc))

    extra = []
    pm = b.get("pathmode")
    if pm == "full":
        extra.append("-spf")
    elif pm == "abs":
        extra.append("-spf2")
    vol = str(b.get("volume") or "").strip().replace(" ", "")
    if vol:
        if not VOL_RE.match(vol) or t not in ("7z", "zip", "tar") or spec.get("tar"):
            return fail("E_BAD_REQUEST")
    user = []
    for tok in shlex.split(str(b.get("params") or ""), posix=not IS_WIN):
        if not tok.startswith("-"):
            return fail("E_PARAMS", tok)
        user.append(tok)

    if spec.get("tar"):
        inner = re.sub(r"\.(gz|bz2|xz)$", "", os.path.basename(out), flags=re.I)
        c1 = ([STATE["exe"], "a", "-ttar", "-so", "-bsp2", "-bso2", "-bse2", "-bb1"] + extra
              + ["--", "pipe.tar"] + sources)
        c2 = ([STATE["exe"], "a", "-t" + t, "-si" + inner, "-bso0", "-bsp0", "-bse2"]
              + comp_switches(t, b) + user + ["--", out])
        jid, _ = start_job(c1, pipe=c2, out=out, parse_sum=True)
        return {"job": jid, "out": out, "open": out}

    cmd = [STATE["exe"], "a", "-t" + t, "-bsp1", "-bb1"] + comp_switches(t, b) + extra
    if updatable and upd in UPDATE_SW:
        cmd.append(UPDATE_SW[upd])
    if vol:
        cmd.append("-v" + vol)
    if pw:
        cmd.append("-p" + pw)
        if t == "zip":
            cmd.append("-mem=" + ("ZipCrypto" if b.get("enc_method") == "ZipCrypto" else "AES256"))
        if t == "7z" and b.get("enc_names"):
            cmd.append("-mhe=on")
    if t == "7z" and b.get("sfx"):
        cmd.append("-sfx")
    if b.get("sdel"):
        cmd.append("-sdel")
    if b.get("shared") and IS_WIN:
        cmd.append("-ssw")
    cmd += user + ["--", out] + sources
    jid, _ = start_job(cmd, out=out, parse_sum=True)
    return {"job": jid, "out": out, "open": out + ".001" if vol else out}


def api_job(q):
    jid = q.get("id", [""])[0]
    since = to_int(q.get("since", ["0"])[0]) or 0
    job = JOBS.get(jid)
    if not job:
        return fail("E_NO_JOB")
    return job.snapshot(since)


def api_pause(b):
    job = JOBS.get(b.get("job", ""))
    if not job:
        return fail("E_NO_JOB")
    ok = job.set_paused(bool(b.get("paused")))
    return {"ok": ok, "paused": job.paused}


def api_background(b):
    job = JOBS.get(b.get("job", ""))
    if not job:
        return fail("E_NO_JOB")
    ok = job.set_background(bool(b.get("on")))
    return {"ok": True, "applied": ok, "background": job.background}


def api_hash(b):
    """Disk uzerindeki dosyalarin veya arsiv icindeki ogelerin sağlama toplamlari."""
    err = need_exe()
    if err:
        return err
    archive = b.get("archive")
    if archive:
        if not os.path.isfile(archive):
            return fail("E_ARC_NOT_FOUND")
        cmd = [STATE["exe"], "t", "-scrc*", "-bsp1", "-bb1"] + pw_args(b.get("password", ""))
        cmd += ["--", archive] + item_args(b)
        jid, _ = start_job(cmd, **sizing(b))
        return {"job": jid}
    paths = [p for p in b.get("paths", []) if isinstance(p, str) and p]
    if not paths:
        return fail("E_NO_FILES")
    for p in paths:
        if not os.path.exists(p):
            return fail("E_NOT_FOUND", p)
    jid, _ = start_job([STATE["exe"], "h", "-scrc*", "-bsp1", "--"] + paths, parse_sum=True)
    return {"job": jid}


def api_hashresult(q):
    job = JOBS.get(q.get("id", [""])[0])
    if not job or not job.done.is_set():
        return fail("E_NOT_READY")
    return parse_hash(job.text())


def api_bench(b):
    err = need_exe()
    if err:
        return err
    cpus = os.cpu_count() or 1
    threads = min(max(to_int(b.get("threads")) or cpus, 1), cpus * 4)
    dlog = min(max(to_int(b.get("dict")) or 24, 18), 31)
    passes = min(max(to_int(b.get("passes")) or 1, 1), 99)
    cmd = [STATE["exe"], "b", f"-mmt={threads}", f"-md={dlog}", str(passes)]
    jid, _ = start_job(cmd, use_pty=True)
    return {"job": jid}


def api_cancel(b):
    job = JOBS.get(b.get("job", ""))
    if job:
        job.cancel()
    return {"ok": True}


def api_reveal(b):
    path = b.get("path", "")
    if not os.path.exists(path):
        return fail("E_OPEN_FAIL")
    try:
        if IS_WIN:
            os.startfile(path)  # noqa: S606
        else:
            opener = "open" if IS_MAC else shutil.which("xdg-open")
            if not opener:
                return fail("E_OPEN_FAIL")
            subprocess.Popen([opener, path], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                             start_new_session=True)
    except OSError:
        return fail("E_OPEN_FAIL")
    return {"ok": True}


def api_quit(_b):
    threading.Thread(target=STATE["server"].shutdown, daemon=True).start()
    return {"ok": True}


GET_ROUTES = {"/api/info": api_info, "/api/ls": api_ls, "/api/job": api_job,
              "/api/hashresult": api_hashresult,
              "/api/ping": lambda q: STATE.update(last_ping=time.time()) or {"ok": True}}
POST_ROUTES = {"/api/open": api_open, "/api/extract": api_extract, "/api/test": api_test,
               "/api/fetch": api_fetch, "/api/create": api_create, "/api/cancel": api_cancel,
               "/api/pause": api_pause, "/api/background": api_background,
               "/api/hash": api_hash, "/api/bench": api_bench,
               "/api/mkdir": api_mkdir, "/api/reveal": api_reveal, "/api/quit": api_quit}


# --------------------------------------------------------------------------
# HTTP sunucusu
# --------------------------------------------------------------------------
class Handler(BaseHTTPRequestHandler):
    server_version = "7zgui"
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def _allowed(self, query):
        host = (self.headers.get("Host") or "").lower()
        if host not in (f"127.0.0.1:{STATE['port']}", f"localhost:{STATE['port']}"):
            return False  # DNS rebinding korumasi
        tok = query.get("t", [""])[0]
        return secrets.compare_digest(tok, STATE["token"])

    def _send(self, code, body, ctype="application/json; charset=utf-8", headers=None):
        if isinstance(body, (dict, list)):
            body = json.dumps(body, ensure_ascii=False).encode("utf-8")
        elif isinstance(body, str):
            body = body.encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        for k, v in (headers or {}).items():
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        u = urlparse(self.path)
        q = parse_qs(u.query)
        if not self._allowed(q):
            return self._send(403, {"error": "Forbidden"})
        if u.path == "/":
            return self._send(200, render_page(), "text/html; charset=utf-8", {
                "Referrer-Policy": "no-referrer",
                "Content-Security-Policy": "default-src 'self'; script-src 'unsafe-inline'; "
                                           "style-src 'unsafe-inline'; img-src 'self' data:"})
        if u.path == "/api/file":
            return self._serve_file(q)
        fn = GET_ROUTES.get(u.path)
        if not fn:
            return self._send(404, {"error": "Not found"})
        try:
            self._send(200, fn(q))
        except Exception as exc:  # noqa: BLE001
            self._send(500, {"error": f"Server error: {exc}"})

    def do_POST(self):
        u = urlparse(self.path)
        q = parse_qs(u.query)
        if not self._allowed(q):
            return self._send(403, {"error": "Forbidden"})
        if "application/json" not in (self.headers.get("Content-Type") or ""):
            return self._send(415, {"error": "JSON required"})
        fn = POST_ROUTES.get(u.path)
        if not fn:
            return self._send(404, {"error": "Not found"})
        try:
            n = int(self.headers.get("Content-Length") or 0)
            body = json.loads(self.rfile.read(n) or b"{}")
            self._send(200, fn(body))
        except Exception as exc:  # noqa: BLE001
            self._send(500, {"error": f"Server error: {exc}"})

    def _serve_file(self, q):
        job = JOBS.get(q.get("id", [""])[0])
        path = job.extra.get("file") if job else None
        if not job or not path or not job.done.is_set() or job.rc not in (0, 1) \
                or not os.path.isfile(path):
            return self._send(404, {"error": "File not ready"})
        name = job.extra.get("name", "dosya")
        ctype = mimetypes.guess_type(name)[0] or "application/octet-stream"
        size = os.path.getsize(path)
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(size))
        self.send_header("Content-Disposition", "attachment; filename*=UTF-8''" + quote(name))
        self.send_header("X-Content-Type-Options", "nosniff")
        self.end_headers()
        with open(path, "rb") as fh:
            shutil.copyfileobj(fh, self.wfile)


class Server(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True


# --------------------------------------------------------------------------
# Arayuz (tek sayfa)
# --------------------------------------------------------------------------
PAGE = r"""<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>7Zip GUI</title>
<style>
/* Themes: no data-theme = follow the system (light/dark); light | dark | oled = user choice */
:root{color-scheme:light dark;
--bg:#f4f5f8;--panel:#fff;--fg:#1c2230;--mut:#6a7388;--bd:#d8dce6;--acc:#2358d6;--accfg:#fff;
--hov:#eef2fc;--sel:#dce7ff;--err:#c62828;--warn:#b26a00;--ok:#2e7d32;--shadow:0 10px 40px rgba(0,0,0,.25);--ovl:rgba(0,0,0,.45)}
@media(prefers-color-scheme:dark){:root:not([data-theme]){
--bg:#13161c;--panel:#1b1f27;--fg:#e5e8ef;--mut:#8d96aa;--bd:#2c3240;
--acc:#5b8def;--accfg:#0b1020;--hov:#232a37;--sel:#263a66;--err:#ef6b6b;--warn:#e0a040;--ok:#6cc070}}
:root[data-theme=light]{color-scheme:light;
--bg:#f4f5f8;--panel:#fff;--fg:#1c2230;--mut:#6a7388;--bd:#d8dce6;--acc:#2358d6;--accfg:#fff;
--hov:#eef2fc;--sel:#dce7ff;--err:#c62828;--warn:#b26a00;--ok:#2e7d32;--shadow:0 10px 40px rgba(0,0,0,.25);--ovl:rgba(0,0,0,.45)}
:root[data-theme=dark]{color-scheme:dark;
--bg:#13161c;--panel:#1b1f27;--fg:#e5e8ef;--mut:#8d96aa;--bd:#2c3240;
--acc:#5b8def;--accfg:#0b1020;--hov:#232a37;--sel:#263a66;--err:#ef6b6b;--warn:#e0a040;--ok:#6cc070;--shadow:0 10px 40px rgba(0,0,0,.6);--ovl:rgba(0,0,0,.55)}
/* OLED: true black background, near-black panels, thin borders so pixels stay off */
:root[data-theme=oled]{color-scheme:dark;
--bg:#000;--panel:#000;--fg:#e8eaf0;--mut:#8a93a6;--bd:#262b36;
--acc:#5b8def;--accfg:#000;--hov:#12161e;--sel:#13233f;--err:#ff7070;--warn:#e8a84a;--ok:#74d078;--shadow:0 0 0 1px #262b36;--ovl:rgba(0,0,0,.78)}
*{box-sizing:border-box}
html,body{height:100%;margin:0}
body{background:var(--bg);color:var(--fg);font:14px/1.4 system-ui,-apple-system,"Segoe UI",Roboto,"Noto Sans","Noto Sans SC","Microsoft YaHei",sans-serif;
display:flex;flex-direction:column}
body.busy,body.busy *{cursor:progress!important}
button,input,select{font:inherit;color:inherit}
select option{background:var(--panel);color:var(--fg)}
.bar-top{display:flex;flex-wrap:wrap;gap:8px;align-items:center;padding:10px 12px;background:var(--panel);border-bottom:1px solid var(--bd)}
.bar-top .sp{flex:1;min-width:0;color:var(--mut);white-space:nowrap;overflow:hidden;text-overflow:ellipsis;padding-left:8px}
.btn{background:var(--panel);border:1px solid var(--bd);border-radius:7px;padding:6px 14px;cursor:pointer}
.btn:hover{background:var(--hov)}
.btn:disabled{opacity:.5;cursor:default}
.btn.primary{background:var(--acc);color:var(--accfg);border-color:var(--acc);font-weight:600}
.btn.primary:hover{filter:brightness(1.08)}
.btn.danger{color:var(--err)}
.btn.sm{padding:4px 10px}
.pref{background:var(--panel);border:1px solid var(--bd);border-radius:7px;padding:5px 6px;cursor:pointer;max-width:150px}
.warnbar{background:var(--err);color:#fff;padding:10px 14px;display:none}
.wrap{flex:1;min-height:0;overflow:auto;background:var(--panel);margin:10px 12px;border:1px solid var(--bd);border-radius:8px;position:relative}
table{border-collapse:collapse;width:100%}
th{position:sticky;top:0;background:var(--panel);text-align:left;font-weight:600;color:var(--mut);padding:7px 8px;
border-bottom:1px solid var(--bd);white-space:nowrap;z-index:1}
td{padding:4px 8px;border-bottom:1px solid color-mix(in srgb,var(--bd) 55%,transparent);white-space:nowrap}
tr:hover td{background:var(--hov)}
tr.sel td{background:var(--sel)}
td.c,th.c{width:34px;text-align:center}
td.r,th.r{text-align:right}
td.nm{width:100%;max-width:0;overflow:hidden;text-overflow:ellipsis;cursor:default}
.dim{color:var(--mut)}
.tw{display:inline-block;width:16px;color:var(--mut);cursor:pointer;user-select:none}
.ic{margin-right:6px}
#empty{position:absolute;inset:0;display:flex;flex-direction:column;align-items:center;justify-content:center;gap:12px;color:var(--mut);text-align:center;padding:20px}
.stat{padding:6px 14px 10px;color:var(--mut);display:flex;gap:16px}
.stat .sp{flex:1}
.ov{position:fixed;inset:0;background:var(--ovl);display:flex;align-items:center;justify-content:center;z-index:50;padding:12px}
.modal{background:var(--panel);border:1px solid var(--bd);border-radius:12px;box-shadow:var(--shadow);max-width:100%;
max-height:94vh;display:flex;flex-direction:column}
.mh{padding:14px 18px;font-weight:700;font-size:15px;border-bottom:1px solid var(--bd)}
.mb{padding:14px 18px;overflow:auto;min-height:0}
.mf{padding:12px 18px;border-top:1px solid var(--bd);display:flex;justify-content:flex-end;gap:8px;flex-wrap:wrap}
.msg{white-space:pre-wrap;word-break:break-word}
.in{width:100%;background:var(--bg);border:1px solid var(--bd);border-radius:7px;padding:7px 9px}
.in:focus{outline:2px solid var(--acc);outline-offset:-1px}
.in:disabled{opacity:.55}
.row{display:flex;gap:8px;align-items:center;margin-bottom:10px}
.row>label{min-width:96px;color:var(--mut)}
.row.top{align-items:flex-start}
.grp{border:1px solid var(--bd);border-radius:8px;padding:8px 12px;margin:0 0 10px;min-width:0}
.grp legend{color:var(--mut);padding:0 6px}
.grp label{display:block;margin:3px 0}
.grp label.dis{opacity:.5}
.fl{height:320px;overflow:auto;border:1px solid var(--bd);border-radius:8px;margin-top:8px;background:var(--bg)}
.fr{display:flex;gap:8px;align-items:center;padding:5px 10px;cursor:pointer;border-bottom:1px solid color-mix(in srgb,var(--bd) 45%,transparent)}
.fr:hover{background:var(--hov)}
.fr.on{background:var(--sel)}
.fr .n{flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.fr .m{color:var(--mut);font-size:12px;white-space:nowrap}
.srcs{border:1px solid var(--bd);border-radius:8px;min-height:56px;max-height:104px;overflow:auto;background:var(--bg);width:100%}
.src{display:flex;align-items:center;gap:6px;padding:4px 10px;border-bottom:1px solid color-mix(in srgb,var(--bd) 45%,transparent)}
.src .sp{flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.src .x{border:0;background:none;cursor:pointer;color:var(--mut)}
.src .x:hover{color:var(--err)}
.mut{color:var(--mut);padding:10px}
.bar{height:10px;background:var(--bg);border:1px solid var(--bd);border-radius:6px;overflow:hidden;margin:10px 0}
.bar .fill{height:100%;width:0;background:var(--acc);transition:width .2s}
.bar.ind .fill{width:35%;animation:ind 1.1s ease-in-out infinite alternate}
@keyframes ind{from{margin-left:0}to{margin-left:65%}}
.log{background:var(--bg);border:1px solid var(--bd);border-radius:8px;padding:8px;height:150px;overflow:auto;margin:0;
font:12px/1.35 ui-monospace,Menlo,Consolas,monospace;white-space:pre-wrap;word-break:break-all}
.plbl{min-height:20px;word-break:break-all}
.plbl.err{color:var(--err);font-weight:600}.plbl.warn{color:var(--warn);font-weight:600}
.pgrid{display:grid;grid-template-columns:1fr 1fr;gap:6px 36px;margin-bottom:14px}
.kv{display:grid;grid-template-columns:auto 1fr;gap:5px 14px;align-content:start}
.kv .v{text-align:right;font-variant-numeric:tabular-nums}
.verb{font-weight:600;margin-top:4px}
.curfile{min-height:2.8em;color:var(--mut);word-break:break-all;line-height:1.4;margin-bottom:6px}
.pwrap{max-height:62vh;overflow:auto;font-variant-numeric:tabular-nums}
.ptab{width:100%}
.ptab td{padding:2px 8px;border:0;white-space:normal}
.ptab td.k{width:42%;color:var(--mut)}
.ptab td.val{word-break:break-all}
.ptab tr:hover td{background:none}
.ptab tr.sep td{border-top:1px dashed var(--bd);padding:0;height:8px}
.hgrid{display:grid;grid-template-columns:auto 1fr;gap:6px 16px}
.hk{color:var(--mut);white-space:nowrap}
.hv{font:12.5px/1.4 ui-monospace,Menlo,Consolas,monospace;word-break:break-all;user-select:all}
.hitems th{position:static;font-size:12px}
.hitems td.hv{font-size:11px}
details{margin-top:12px}
summary{cursor:pointer;color:var(--mut)}
.bgrid{display:grid;grid-template-columns:1fr 230px;gap:16px}
.btab{width:100%;font-variant-numeric:tabular-nums}
.btab th{position:static;font-size:12px;text-align:right;padding:3px 6px}
.btab td{text-align:right;border:0;padding:3px 6px}
.btab td.k{text-align:left;color:var(--mut)}
.ttab{display:grid;grid-template-columns:repeat(3,auto);gap:4px 16px;text-align:right;font-variant-numeric:tabular-nums}
.ttab .th{color:var(--mut);font-size:12px}
.freq{font:12px/1.5 ui-monospace,Menlo,Consolas,monospace;color:var(--mut);margin:0;white-space:pre-wrap;word-break:break-all;overflow:auto;max-height:430px}
.binfo{text-align:center;color:var(--mut);margin-top:8px;font-size:12.5px;line-height:1.6}
.rawlog{font:11px/1.35 ui-monospace,Menlo,Consolas,monospace;max-height:160px;overflow:auto;background:var(--bg);border:1px solid var(--bd);border-radius:8px;padding:6px;margin:6px 0 0;white-space:pre-wrap}
/* Add to Archive dialog */
.cgrid{display:grid;grid-template-columns:1fr 1fr;gap:0 22px}
.cf{display:grid;grid-template-columns:minmax(120px,42%) 1fr;gap:6px 10px;align-items:center;margin-bottom:5px}
.cf>label{color:var(--mut)}
.cf>label.dis{opacity:.5}
.cf .in{padding:4px 8px}
.cwarn{color:var(--warn);margin:0 0 10px;display:none;font-size:13px}
@media(max-width:760px){.bgrid,.pgrid,.cgrid{grid-template-columns:1fr}}
</style></head>
<body>
<div class="warnbar" id="warn"></div>
<div class="bar-top">
  <button class="btn primary" id="bNew" data-i18n="bNew"></button>
  <button class="btn" id="bOpen" data-i18n="bOpen"></button>
  <button class="btn" id="bExtract" data-i18n="bExtract"></button>
  <button class="btn" id="bTest" data-i18n="bTest"></button>
  <button class="btn" id="bInfo" data-i18n="bInfo"></button>
  <button class="btn" id="bHash" data-i18n="bHash" title="CRC32, CRC64, SHA-256, SHA-1, BLAKE2sp …"></button>
  <button class="btn" id="bBench" data-i18n="bBench"></button>
  <span class="sp" id="title"></span>
  <select class="pref" id="selTheme" data-i18n-title="theme"></select>
  <select class="pref" id="selLang" data-i18n-title="lang"></select>
  <button class="btn danger" id="bQuit" data-i18n="bQuit" data-i18n-title="quitTip"></button>
</div>
<div class="wrap" id="wrap">
  <table id="tbl" style="display:none">
    <thead><tr><th class="c"><input type="checkbox" id="all"></th><th data-i18n="colName"></th><th class="r" data-i18n="colSize"></th>
    <th class="r" data-i18n="colPacked"></th><th data-i18n="colModified"></th><th data-i18n="colAttr"></th><th data-i18n="colEnc"></th></tr></thead>
    <tbody id="rows"></tbody>
  </table>
  <div id="empty"><div style="font-size:42px">🗜️</div>
    <div data-i18n="emptyMsg"></div>
    <div class="dim" id="ver"></div></div>
</div>
<div class="stat"><span id="status"></span><span class="sp"></span><span id="selinfo"></span></div>
<div id="modals"></div>
<script>
(function(){
'use strict';
const INITIAL = /*__INITIAL__*/null;
/* ===================== i18n: en, tr, de, fr, es, ru, zh ===================== */
const LANGS = [['en', 'English'], ['tr', 'Türkçe'], ['de', 'Deutsch'], ['fr', 'Français'], ['es', 'Español'], ['ru', 'Русский'], ['zh', '简体中文']];
const D = {}; LANGS.forEach(([c]) => { D[c] = {}; });
function K(key, ...v){ LANGS.forEach(([c], i) => { D[c][key] = v[i] != null ? v[i] : v[0]; }); }

/* --- toolbar / main window --- */
K('bNew', 'New archive', 'Yeni arşiv', 'Neues Archiv', 'Nouvelle archive', 'Nuevo archivo', 'Новый архив', '新建压缩包');
K('bOpen', 'Open archive', 'Arşiv aç', 'Archiv öffnen', 'Ouvrir', 'Abrir archivo', 'Открыть архив', '打开压缩包');
K('bExtract', 'Extract', 'Çıkar', 'Entpacken', 'Extraire', 'Extraer', 'Извлечь', '解压');
K('bTest', 'Test', 'Test', 'Testen', 'Tester', 'Probar', 'Проверить', '测试');
K('bInfo', 'Info', 'Bilgi', 'Info', 'Infos', 'Info', 'Сведения', '信息');
K('bHash', 'Checksum', 'Sağlama toplamı', 'Prüfsumme', 'Somme de contrôle', 'Suma de verificación', 'Контрольная сумма', '校验和');
K('bBench', 'Benchmark', 'Benchmark', 'Benchmark', 'Benchmark', 'Benchmark', 'Тест скорости', '基准测试');
K('bQuit', 'Quit', 'Kapat', 'Beenden', 'Quitter', 'Salir', 'Выход', '退出');
K('quitTip', 'Quit the program', 'Programı kapat', 'Programm beenden', 'Quitter le programme', 'Cerrar el programa', 'Закрыть программу', '退出程序');
K('theme', 'Theme', 'Tema', 'Design', 'Thème', 'Tema', 'Тема', '主题');
K('themeAuto', 'System (auto)', 'Sistem (otomatik)', 'System (automatisch)', 'Système (auto)', 'Sistema (auto)', 'Системная (авто)', '跟随系统');
K('themeLight', 'Light', 'Aydınlık', 'Hell', 'Clair', 'Claro', 'Светлая', '浅色');
K('themeDark', 'Dark', 'Koyu', 'Dunkel', 'Sombre', 'Oscuro', 'Тёмная', '深色');
K('themeOled', 'OLED black', 'OLED (tam siyah)', 'OLED (Schwarz)', 'OLED (noir pur)', 'OLED (negro)', 'OLED (чёрная)', 'OLED 纯黑');
K('lang', 'Language', 'Dil', 'Sprache', 'Langue', 'Idioma', 'Язык', '语言');
K('colName', 'Name', 'Ad', 'Name', 'Nom', 'Nombre', 'Имя', '名称');
K('colSize', 'Size', 'Boyut', 'Größe', 'Taille', 'Tamaño', 'Размер', '大小');
K('colPacked', 'Packed', 'Sıkıştırılmış', 'Gepackt', 'Compressé', 'Comprimido', 'Сжатый', '压缩后');
K('colModified', 'Modified', 'Değiştirilme', 'Geändert', 'Modifié', 'Modificado', 'Изменён', '修改时间');
K('colAttr', 'Attributes', 'Öznitelik', 'Attribute', 'Attributs', 'Atributos', 'Атрибуты', '属性');
K('colEnc', 'Encrypted', 'Şifreli', 'Verschlüsselt', 'Chiffré', 'Cifrado', 'Шифр.', '加密');
K('yes', 'Yes', 'Evet', 'Ja', 'Oui', 'Sí', 'Да', '是');
K('emptyMsg', 'Click "Open archive" to open an archive, or "New archive" to create one.',
  'Bir arşiv açmak için "Arşiv aç", yeni arşiv için "Yeni arşiv" düğmesine basın.',
  'Klicken Sie auf „Archiv öffnen“ oder „Neues Archiv“.',
  'Cliquez sur « Ouvrir » pour ouvrir une archive ou sur « Nouvelle archive » pour en créer une.',
  'Pulse «Abrir archivo» para abrir un archivo o «Nuevo archivo» para crear uno.',
  'Нажмите «Открыть архив», чтобы открыть архив, или «Новый архив», чтобы создать его.',
  '点击“打开压缩包”打开压缩包，或点击“新建压缩包”创建。');
K('ready', 'Ready', 'Hazır', 'Bereit', 'Prêt', 'Listo', 'Готово', '就绪');
K('readyWith', 'Ready — {0}', 'Hazır — {0}', 'Bereit — {0}', 'Prêt — {0}', 'Listo — {0}', 'Готово — {0}', '就绪 — {0}');
K('reading', 'Reading archive…', 'Arşiv okunuyor…', 'Archiv wird gelesen …', 'Lecture de l’archive…', 'Leyendo archivo…', 'Чтение архива…', '正在读取压缩包…');
K('cancelled', 'Cancelled.', 'İptal edildi.', 'Abgebrochen.', 'Annulé.', 'Cancelado.', 'Отменено.', '已取消。');
K('openFailed', 'Could not open the archive.', 'Arşiv açılamadı.', 'Archiv konnte nicht geöffnet werden.', 'Impossible d’ouvrir l’archive.', 'No se pudo abrir el archivo.', 'Не удалось открыть архив.', '无法打开压缩包。');
K('statusArc', '{0} files  |  unpacked size: {1}  |  archive size: {2}', '{0} dosya  |  açılmış boyut: {1}  |  arşiv boyutu: {2}',
  '{0} Dateien  |  entpackte Größe: {1}  |  Archivgröße: {2}', '{0} fichiers  |  taille décompressée : {1}  |  taille de l’archive : {2}',
  '{0} archivos  |  tamaño descomprimido: {1}  |  tamaño del archivo: {2}', '{0} файлов  |  распакованный размер: {1}  |  размер архива: {2}', '{0} 个文件  |  解压后大小：{1}  |  压缩包大小：{2}');
K('selCount', '{0} items selected', '{0} öğe seçili', '{0} Elemente ausgewählt', '{0} éléments sélectionnés', '{0} elementos seleccionados', 'Выбрано элементов: {0}', '已选择 {0} 项');
K('no7z', '7-Zip not found. Put the 7zz / 7z.exe file into the same folder as this program ({0}) and restart.',
  '7-Zip bulunamadı. 7zz / 7z.exe dosyasını bu programla aynı klasöre ({0}) koyun ve yeniden başlatın.',
  '7-Zip wurde nicht gefunden. Legen Sie 7zz / 7z.exe in denselben Ordner wie dieses Programm ({0}) und starten Sie neu.',
  '7-Zip introuvable. Placez 7zz / 7z.exe dans le même dossier que ce programme ({0}) puis relancez.',
  'No se encontró 7-Zip. Coloque 7zz / 7z.exe en la misma carpeta que este programa ({0}) y reinicie.',
  '7-Zip не найден. Поместите 7zz / 7z.exe в одну папку с программой ({0}) и перезапустите.',
  '未找到 7-Zip。请将 7zz / 7z.exe 放入本程序所在文件夹（{0}）后重新启动。');
K('no7zShort', '7-Zip not found', '7-Zip bulunamadı', '7-Zip nicht gefunden', '7-Zip introuvable', '7-Zip no encontrado', '7-Zip не найден', '未找到 7-Zip');
K('noServer', 'Cannot reach the server.', 'Sunucuya ulaşılamıyor.', 'Server nicht erreichbar.', 'Serveur injoignable.', 'No se puede acceder al servidor.', 'Сервер недоступен.', '无法连接到服务器。');

/* --- generic --- */
K('ok', 'OK', 'Tamam', 'OK', 'OK', 'Aceptar', 'ОК', '确定');
K('cancel', 'Cancel', 'İptal', 'Abbrechen', 'Annuler', 'Cancelar', 'Отмена', '取消');
K('close', 'Close', 'Kapat', 'Schließen', 'Fermer', 'Cerrar', 'Закрыть', '关闭');
K('browse', 'Browse…', 'Gözat…', 'Durchsuchen …', 'Parcourir…', 'Examinar…', 'Обзор…', '浏览…');
K('password', 'Password', 'Parola', 'Passwort', 'Mot de passe', 'Contraseña', 'Пароль', '密码');
K('openFirst', 'Open an archive first.', 'Önce bir arşiv açın.', 'Öffnen Sie zuerst ein Archiv.', 'Ouvrez d’abord une archive.', 'Abra primero un archivo.', 'Сначала откройте архив.', '请先打开一个压缩包。');
K('quitAsk', 'Quit the program?', 'Program kapatılsın mı?', 'Programm beenden?', 'Quitter le programme ?', '¿Cerrar el programa?', 'Закрыть программу?', '要退出程序吗？');
K('quitDone', '7Zip GUI has closed. You can close this tab.', '7Zip GUI kapatıldı. Bu sekmeyi kapatabilirsiniz.', '7Zip GUI wurde beendet. Sie können diesen Tab schließen.', '7Zip GUI est fermé. Vous pouvez fermer cet onglet.', '7Zip GUI se cerró. Puede cerrar esta pestaña.', '7Zip GUI закрыт. Эту вкладку можно закрыть.', '7Zip GUI 已关闭，可以关闭此标签页。');
K('pwTitle', 'Password', 'Parola', 'Passwort', 'Mot de passe', 'Contraseña', 'Пароль', '密码');
K('pwNeeded', 'This archive is password protected. Enter the password:', 'Arşiv parola korumalı. Parolayı girin:', 'Dieses Archiv ist passwortgeschützt. Passwort eingeben:', 'Cette archive est protégée par mot de passe. Saisissez-le :', 'Este archivo está protegido con contraseña. Introdúzcala:', 'Архив защищён паролем. Введите пароль:', '此压缩包受密码保护，请输入密码：');
K('pwWrong', 'Wrong password. Try again:', 'Parola yanlış. Tekrar deneyin:', 'Falsches Passwort. Erneut versuchen:', 'Mot de passe incorrect. Réessayez :', 'Contraseña incorrecta. Inténtelo de nuevo:', 'Неверный пароль. Попробуйте ещё раз:', '密码错误，请重试：');
K('arcOpenFail', 'Could not open the archive:\n\n{0}', 'Arşiv açılamadı:\n\n{0}', 'Archiv konnte nicht geöffnet werden:\n\n{0}', 'Impossible d’ouvrir l’archive :\n\n{0}', 'No se pudo abrir el archivo:\n\n{0}', 'Не удалось открыть архив:\n\n{0}', '无法打开压缩包：\n\n{0}');
K('pickArc', 'Choose archive', 'Arşiv seç', 'Archiv auswählen', 'Choisir une archive', 'Elegir archivo', 'Выберите архив', '选择压缩包');

/* --- file picker --- */
K('pickUp', 'Parent folder', 'Üst klasör', 'Übergeordneter Ordner', 'Dossier parent', 'Carpeta superior', 'Родительская папка', '上级文件夹');
K('pickHome', 'Home folder', 'Ev klasörü', 'Home-Ordner', 'Dossier personnel', 'Carpeta personal', 'Домашняя папка', '主文件夹');
K('showHidden', 'Show hidden files', 'Gizli dosyaları göster', 'Versteckte Dateien anzeigen', 'Afficher les fichiers cachés', 'Mostrar archivos ocultos', 'Показать скрытые файлы', '显示隐藏文件');
K('emptyFolder', 'Folder is empty.', 'Klasör boş.', 'Ordner ist leer.', 'Le dossier est vide.', 'La carpeta está vacía.', 'Папка пуста.', '文件夹为空。');
K('drives', 'Drives', 'Sürücüler', 'Laufwerke', 'Lecteurs', 'Unidades', 'Диски', '驱动器');
K('btnOpen', 'Open', 'Aç', 'Öffnen', 'Ouvrir', 'Abrir', 'Открыть', '打开');
K('btnAdd', 'Add', 'Ekle', 'Hinzufügen', 'Ajouter', 'Añadir', 'Добавить', '添加');
K('newFolder', 'New folder…', 'Yeni klasör…', 'Neuer Ordner …', 'Nouveau dossier…', 'Nueva carpeta…', 'Новая папка…', '新建文件夹…');
K('newFolderTitle', 'New folder', 'Yeni klasör', 'Neuer Ordner', 'Nouveau dossier', 'Nueva carpeta', 'Новая папка', '新建文件夹');
K('create', 'Create', 'Oluştur', 'Erstellen', 'Créer', 'Crear', 'Создать', '创建');
K('selectFolder', 'Select this folder', 'Bu klasörü seç', 'Diesen Ordner wählen', 'Choisir ce dossier', 'Elegir esta carpeta', 'Выбрать эту папку', '选择此文件夹');

/* --- extract / test --- */
K('exTitle', 'Extract', 'Çıkar', 'Entpacken', 'Extraire', 'Extraer', 'Извлечь', '解压');
K('exDest', 'Destination folder', 'Hedef klasör', 'Zielordner', 'Dossier de destination', 'Carpeta de destino', 'Папка назначения', '目标文件夹');
K('exPickDest', 'Choose destination folder', 'Hedef klasörü seç', 'Zielordner auswählen', 'Choisir le dossier de destination', 'Elegir carpeta de destino', 'Выберите папку назначения', '选择目标文件夹');
K('exScope', 'Scope', 'Kapsam', 'Umfang', 'Portée', 'Alcance', 'Область', '范围');
K('exAll', 'All files', 'Tüm dosyalar', 'Alle Dateien', 'Tous les fichiers', 'Todos los archivos', 'Все файлы', '全部文件');
K('exSel', 'Selected only ({0})', 'Yalnızca seçilenler ({0})', 'Nur Auswahl ({0})', 'Sélection uniquement ({0})', 'Solo la selección ({0})', 'Только выбранные ({0})', '仅所选项目（{0}）');
K('exIfExists', 'If a file exists', 'Aynı adlıysa', 'Wenn Datei existiert', 'Si le fichier existe', 'Si el archivo existe', 'Если файл существует', '文件已存在时');
K('ow1', 'Overwrite existing files', 'Mevcut dosyaların üzerine yaz', 'Vorhandene überschreiben', 'Écraser les fichiers existants', 'Sobrescribir existentes', 'Перезаписать существующие', '覆盖现有文件');
K('ow2', 'Skip existing files', 'Mevcut dosyaları atla', 'Vorhandene überspringen', 'Ignorer les fichiers existants', 'Omitir existentes', 'Пропустить существующие', '跳过现有文件');
K('ow3', 'Rename new file', 'Yeni dosyayı yeniden adlandır', 'Neue Datei umbenennen', 'Renommer le nouveau fichier', 'Renombrar el archivo nuevo', 'Переименовать новый файл', '重命名新文件');
K('ow4', 'Rename existing file', 'Mevcut dosyayı yeniden adlandır', 'Vorhandene umbenennen', 'Renommer le fichier existant', 'Renombrar el existente', 'Переименовать существующий', '重命名现有文件');
K('exKeep', 'Keep folder structure', 'Klasör yapısını koru', 'Ordnerstruktur beibehalten', 'Conserver l’arborescence', 'Mantener estructura de carpetas', 'Сохранять структуру папок', '保留文件夹结构');
K('exDone', 'Extraction finished.', 'Çıkarma tamamlandı.', 'Entpacken abgeschlossen.', 'Extraction terminée.', 'Extracción finalizada.', 'Извлечение завершено.', '解压完成。');
K('exOpenFolder', 'Open folder', 'Klasörü aç', 'Ordner öffnen', 'Ouvrir le dossier', 'Abrir carpeta', 'Открыть папку', '打开文件夹');
K('exStatus', 'Extraction finished: {0}', 'Çıkarma tamamlandı: {0}', 'Entpacken abgeschlossen: {0}', 'Extraction terminée : {0}', 'Extracción finalizada: {0}', 'Извлечение завершено: {0}', '解压完成：{0}');
K('testOk', 'Test passed: no errors found.', 'Test başarılı: hata bulunamadı.', 'Test bestanden: keine Fehler gefunden.', 'Test réussi : aucune erreur.', 'Prueba superada: no se encontraron errores.', 'Проверка пройдена: ошибок нет.', '测试通过：未发现错误。');

/* --- progress window --- */
K('pElapsed', 'Elapsed time', 'Geçen süre', 'Verstrichene Zeit', 'Temps écoulé', 'Tiempo transcurrido', 'Прошло времени', '已用时间');
K('pRemain', 'Remaining time', 'Kalan süre', 'Verbleibende Zeit', 'Temps restant', 'Tiempo restante', 'Осталось времени', '剩余时间');
K('pFiles', 'Files', 'Dosyalar', 'Dateien', 'Fichiers', 'Archivos', 'Файлы', '文件数');
K('pTotal', 'Total size', 'Toplam boyut', 'Gesamtgröße', 'Taille totale', 'Tamaño total', 'Общий размер', '总大小');
K('pSpeed', 'Speed', 'Hız', 'Geschwindigkeit', 'Vitesse', 'Velocidad', 'Скорость', '速度');
K('pProc', 'Processed', 'İşlenen', 'Verarbeitet', 'Traité', 'Procesado', 'Обработано', '已处理');
K('pComp', 'Compressed size', 'Sıkıştırılmış boyut', 'Komprimierte Größe', 'Taille compressée', 'Tamaño comprimido', 'Сжатый размер', '压缩后大小');
K('pRatio', 'Compression ratio', 'Sıkıştırma oranı', 'Kompressionsrate', 'Taux de compression', 'Relación de compresión', 'Степень сжатия', '压缩率');
K('bBg', 'Background', 'Arka plan', 'Hintergrund', 'Arrière-plan', 'Segundo plano', 'Фоновый', '后台');
K('bFg', 'Foreground', 'Ön plan', 'Vordergrund', 'Premier plan', 'Primer plano', 'Обычный', '前台');
K('bPause', 'Pause', 'Duraklat', 'Pause', 'Pause', 'Pausa', 'Пауза', '暂停');
K('bResume', 'Resume', 'Devam', 'Fortsetzen', 'Reprendre', 'Reanudar', 'Продолжить', '继续');
K('paused', 'Paused', 'Duraklatıldı', 'Pausiert', 'En pause', 'En pausa', 'Приостановлено', '已暂停');
K('doneWarn', 'Completed with warnings', 'Uyarılarla tamamlandı', 'Mit Warnungen abgeschlossen', 'Terminé avec des avertissements', 'Finalizado con advertencias', 'Завершено с предупреждениями', '完成但有警告');
K('doneErr', 'Error', 'Hata oluştu', 'Fehler', 'Erreur', 'Error', 'Ошибка', '出错');
K('noteWarn', 'Some items could not be processed. Details below.', 'Bazı öğeler işlenemedi. Ayrıntılar aşağıda.', 'Einige Elemente konnten nicht verarbeitet werden. Details unten.', 'Certains éléments n’ont pas pu être traités. Détails ci-dessous.', 'Algunos elementos no se pudieron procesar. Detalles abajo.', 'Некоторые элементы не удалось обработать. Подробности ниже.', '部分项目无法处理，详情见下方。');
K('noteErr', 'Operation failed (exit code {0}). Details below.', 'İşlem başarısız (çıkış kodu {0}). Ayrıntılar aşağıda.', 'Vorgang fehlgeschlagen (Exitcode {0}). Details unten.', 'Échec de l’opération (code {0}). Détails ci-dessous.', 'La operación falló (código {0}). Detalles abajo.', 'Операция не удалась (код {0}). Подробности ниже.', '操作失败（退出码 {0}），详情见下方。');
K('prioFail', 'Could not change priority (permission may be required).', 'Öncelik değiştirilemedi (yetki gerekebilir).', 'Priorität konnte nicht geändert werden (ggf. Berechtigung nötig).', 'Impossible de changer la priorité (droits requis ?).', 'No se pudo cambiar la prioridad (puede requerir permisos).', 'Не удалось изменить приоритет (возможно, нужны права).', '无法更改优先级（可能需要权限）。');
K('vExtract', 'Extracting', 'Çıkarılıyor', 'Wird entpackt', 'Extraction', 'Extrayendo', 'Извлечение', '正在解压');
K('vAdd', 'Adding', 'Ekleniyor', 'Wird hinzugefügt', 'Ajout', 'Añadiendo', 'Добавление', '正在添加');
K('vTest', 'Testing', 'Test ediliyor', 'Wird getestet', 'Test', 'Probando', 'Проверка', '正在测试');
K('vHash', 'Calculating', 'Hesaplanıyor', 'Wird berechnet', 'Calcul', 'Calculando', 'Вычисление', '正在计算');
K('vWork', 'Working', 'Çalışıyor', 'In Arbeit', 'En cours', 'Trabajando', 'Выполнение', '处理中');
K('tExtract', 'Extracting…', 'Çıkarılıyor…', 'Entpacken …', 'Extraction…', 'Extrayendo…', 'Извлечение…', '正在解压…');
K('tCreate', 'Creating archive…', 'Arşiv oluşturuluyor…', 'Archiv wird erstellt …', 'Création de l’archive…', 'Creando archivo…', 'Создание архива…', '正在创建压缩包…');
K('tTest', 'Testing archive…', 'Arşiv test ediliyor…', 'Archiv wird getestet …', 'Test de l’archive…', 'Probando archivo…', 'Проверка архива…', '正在测试压缩包…');
K('tHash', 'Calculating checksum…', 'Sağlama toplamı hesaplanıyor…', 'Prüfsumme wird berechnet …', 'Calcul de la somme de contrôle…', 'Calculando suma de verificación…', 'Вычисление контрольной суммы…', '正在计算校验和…');
K('tFetch', 'Preparing file…', 'Dosya hazırlanıyor…', 'Datei wird vorbereitet …', 'Préparation du fichier…', 'Preparando archivo…', 'Подготовка файла…', '正在准备文件…');

/* --- properties --- */
K('propTitle', 'Properties', 'Özellikler', 'Eigenschaften', 'Propriétés', 'Propiedades', 'Свойства', '属性');
K('pSize', 'Size', 'Boyut', 'Größe', 'Taille', 'Tamaño', 'Размер', '大小');
K('pPacked', 'Packed size', 'Sıkıştırılmış boyut', 'Gepackte Größe', 'Taille compressée', 'Tamaño comprimido', 'Сжатый размер', '压缩后大小');
K('pFolders', 'Folders', 'Klasörler', 'Ordner', 'Dossiers', 'Carpetas', 'Папки', '文件夹');
K('pFilesN', 'Files', 'Dosyalar', 'Dateien', 'Fichiers', 'Archivos', 'Файлы', '文件');
K('k.Path', 'Path', 'Yol', 'Pfad', 'Chemin', 'Ruta', 'Путь', '路径');
K('k.Type', 'Type', 'Tür', 'Typ', 'Type', 'Tipo', 'Тип', '类型');
K('k.Physical Size', 'Physical size', 'Fiziksel boyut', 'Physische Größe', 'Taille physique', 'Tamaño físico', 'Физический размер', '物理大小');
K('k.Headers Size', 'Headers size', 'Başlık boyutu', 'Header-Größe', 'Taille des en-têtes', 'Tamaño de cabeceras', 'Размер заголовков', '头部大小');
K('k.Method', 'Method', 'Yöntem', 'Methode', 'Méthode', 'Método', 'Метод', '方法');
K('k.Solid', 'Solid', 'Katı (solid)', 'Solide', 'Solide', 'Sólido', 'Непрерывный', '固实');
K('k.Blocks', 'Blocks', 'Bloklar', 'Blöcke', 'Blocs', 'Bloques', 'Блоки', '块数');
K('k.Comment', 'Comment', 'Açıklama', 'Kommentar', 'Commentaire', 'Comentario', 'Комментарий', '注释');
K('k.Encrypted', 'Encrypted', 'Şifreli', 'Verschlüsselt', 'Chiffré', 'Cifrado', 'Зашифровано', '已加密');
K('k.Warnings', 'Warnings', 'Uyarılar', 'Warnungen', 'Avertissements', 'Advertencias', 'Предупреждения', '警告');
K('k.Errors', 'Errors', 'Hatalar', 'Fehler', 'Erreurs', 'Errores', 'Ошибки', '错误');
K('k.Offset', 'Offset', 'Ofset', 'Offset', 'Décalage', 'Desplazamiento', 'Смещение', '偏移');

/* --- checksum --- */
K('hTitle', 'Checksum information', 'Sağlama toplamı bilgisi', 'Prüfsummen-Information', 'Informations de somme de contrôle', 'Información de suma de verificación', 'Контрольные суммы', '校验和信息');
K('hChoose', 'Calculates CRC32, CRC64, SHA-256, SHA-1, BLAKE2sp and the other checksums supported by 7-Zip.\n\nChoose the source:',
  'CRC32, CRC64, SHA-256, SHA-1, BLAKE2sp ve 7-Zip’in desteklediği diğer sağlama toplamları hesaplanır.\n\nKaynağı seçin:',
  'Berechnet CRC32, CRC64, SHA-256, SHA-1, BLAKE2sp und weitere von 7-Zip unterstützte Prüfsummen.\n\nQuelle wählen:',
  'Calcule CRC32, CRC64, SHA-256, SHA-1, BLAKE2sp et les autres sommes prises en charge par 7-Zip.\n\nChoisissez la source :',
  'Calcula CRC32, CRC64, SHA-256, SHA-1, BLAKE2sp y otras sumas compatibles con 7-Zip.\n\nElija el origen:',
  'Вычисляет CRC32, CRC64, SHA-256, SHA-1, BLAKE2sp и другие суммы, поддерживаемые 7-Zip.\n\nВыберите источник:',
  '计算 CRC32、CRC64、SHA-256、SHA-1、BLAKE2sp 以及 7-Zip 支持的其他校验和。\n\n请选择来源：');
K('hSrcSel', 'Selected items in archive ({0})', 'Arşivde seçili öğeler ({0})', 'Ausgewählte Elemente im Archiv ({0})', 'Éléments sélectionnés de l’archive ({0})', 'Elementos seleccionados del archivo ({0})', 'Выбранные элементы архива ({0})', '压缩包中所选项目（{0}）');
K('hSrcAll', 'Whole archive contents', 'Arşivin tüm içeriği', 'Gesamter Archivinhalt', 'Tout le contenu de l’archive', 'Todo el contenido del archivo', 'Всё содержимое архива', '压缩包全部内容');
K('hSrcDisk', 'File / folder on disk…', 'Diskten dosya / klasör…', 'Datei / Ordner auf Datenträger …', 'Fichier / dossier sur le disque…', 'Archivo / carpeta del disco…', 'Файл / папка на диске…', '磁盘上的文件/文件夹…');
K('hPick', 'Choose files / folders for the checksum', 'Sağlama toplamı için dosya / klasör seç', 'Dateien / Ordner für die Prüfsumme wählen', 'Choisir les fichiers / dossiers', 'Elegir archivos / carpetas', 'Выберите файлы / папки', '选择要计算校验和的文件/文件夹');
K('hName', 'Name', 'Ad', 'Name', 'Nom', 'Nombre', 'Имя', '名称');
K('hBytes', '{0} bytes : {1} KiB', '{0} bayt : {1} KiB', '{0} Bytes : {1} KiB', '{0} octets : {1} Kio', '{0} bytes : {1} KiB', '{0} байт : {1} КиБ', '{0} 字节 : {1} KiB');
K('hData', '{0} (data)', '{0} (veri)', '{0} (Daten)', '{0} (données)', '{0} (datos)', '{0} (данные)', '{0}（数据）');
K('hDataNames', '{0} (data and names)', '{0} (veri ve adlar)', '{0} (Daten und Namen)', '{0} (données et noms)', '{0} (datos y nombres)', '{0} (данные и имена)', '{0}（数据和名称）');
K('hPerFile', 'Per-file checksums ({0})', 'Dosya başına sağlama toplamları ({0})', 'Prüfsummen pro Datei ({0})', 'Sommes par fichier ({0})', 'Sumas por archivo ({0})', 'Суммы по файлам ({0})', '各文件校验和（{0}）');
K('hNoResult', 'Could not read the result.', 'Sonuç okunamadı.', 'Ergebnis konnte nicht gelesen werden.', 'Impossible de lire le résultat.', 'No se pudo leer el resultado.', 'Не удалось прочитать результат.', '无法读取结果。');
K('copy', 'Copy', 'Kopyala', 'Kopieren', 'Copier', 'Copiar', 'Копировать', '复制');
K('copied', 'Copied ✓', 'Kopyalandı ✓', 'Kopiert ✓', 'Copié ✓', 'Copiado ✓', 'Скопировано ✓', '已复制 ✓');
K('copyFail', 'Copy failed', 'Kopyalanamadı', 'Kopieren fehlgeschlagen', 'Échec de la copie', 'Error al copiar', 'Не удалось скопировать', '复制失败');

/* --- benchmark --- */
K('bnTitle', 'Benchmark', 'Benchmark', 'Benchmark', 'Benchmark', 'Benchmark', 'Тест производительности', '基准测试');
K('bnDict', 'Dictionary size', 'Sözlük boyutu', 'Wörterbuchgröße', 'Taille du dictionnaire', 'Tamaño del diccionario', 'Размер словаря', '字典大小');
K('bnThreads', 'CPU threads', 'CPU iş parçacığı', 'CPU-Threads', 'Threads CPU', 'Hilos de CPU', 'Потоки ЦП', 'CPU 线程数');
K('bnMem', 'Memory usage', 'Bellek kullanımı', 'Speicherverbrauch', 'Utilisation mémoire', 'Uso de memoria', 'Использование памяти', '内存使用');
K('bnComp', 'Compressing', 'Sıkıştırma', 'Komprimieren', 'Compression', 'Compresión', 'Сжатие', '压缩');
K('bnDec', 'Decompressing', 'Açma', 'Dekomprimieren', 'Décompression', 'Descompresión', 'Распаковка', '解压');
K('bnCur', 'Current', 'Güncel', 'Aktuell', 'Actuel', 'Actual', 'Текущий', '当前');
K('bnRes', 'Resulting', 'Sonuç', 'Ergebnis', 'Résultat', 'Resultado', 'Итог', '结果');
K('bnCDict', 'Dictionary', 'Sözlük', 'Wörterbuch', 'Dictionnaire', 'Diccionario', 'Словарь', '字典');
K('bnCSpeed', 'Speed', 'Hız', 'Geschw.', 'Vitesse', 'Velocidad', 'Скорость', '速度');
K('bnCUsage', 'CPU usage', 'CPU kullanımı', 'CPU-Last', 'Usage CPU', 'Uso de CPU', 'Загрузка ЦП', 'CPU 占用');
K('bnCRU', 'Rating / Usage', 'Oran / Kullanım', 'Wertung / Last', 'Note / Usage', 'Valor / Uso', 'Рейтинг / Загр.', '评分/占用');
K('bnCRating', 'Rating', 'Derecelendirme', 'Wertung', 'Note', 'Valoración', 'Рейтинг', '评分');
K('bnElapsed', 'Elapsed time', 'Geçen süre', 'Verstrichene Zeit', 'Temps écoulé', 'Tiempo transcurrido', 'Прошло времени', '已用时间');
K('bnPasses', 'Passes', 'Tur sayısı', 'Durchläufe', 'Passes', 'Pasadas', 'Проходы', '遍数');
K('bnTotal', 'Total rating', 'Toplam derecelendirme', 'Gesamtwertung', 'Note totale', 'Valoración total', 'Общий рейтинг', '总评分');
K('bnStart', 'Start', 'Başlat', 'Start', 'Démarrer', 'Iniciar', 'Старт', '开始');
K('bnRestart', 'Restart', 'Yeniden başlat', 'Neu starten', 'Redémarrer', 'Reiniciar', 'Перезапуск', '重新开始');
K('bnStop', 'Stop', 'Durdur', 'Stopp', 'Arrêter', 'Detener', 'Стоп', '停止');
K('bnRaw', 'Raw output', 'Ham çıktı', 'Rohausgabe', 'Sortie brute', 'Salida sin procesar', 'Исходный вывод', '原始输出');
K('bnErr', 'Benchmark failed (code {0}). See “Raw output”.', 'Benchmark hata ile bitti (kod {0}). Ayrıntı için “Ham çıktı” bölümüne bakın.', 'Benchmark fehlgeschlagen (Code {0}). Siehe „Rohausgabe“.', 'Échec du benchmark (code {0}). Voir « Sortie brute ».', 'El benchmark falló (código {0}). Vea «Salida sin procesar».', 'Тест завершился с ошибкой (код {0}). См. «Исходный вывод».', '基准测试失败（代码 {0}），请查看“原始输出”。');

/* --- Add to Archive dialog --- */
K('cTitle', 'Add to Archive', 'Arşive Ekle', 'Zum Archiv hinzufügen', 'Ajouter à l’archive', 'Añadir al archivo', 'Добавить в архив', '添加到压缩包');
K('cItems', 'Items to add:', 'Eklenecek dosya ve klasörler:', 'Hinzuzufügende Elemente:', 'Éléments à ajouter :', 'Elementos a añadir:', 'Что добавить:', '要添加的项目：');
K('cAddMore', 'Add…', 'Ekle…', 'Hinzufügen …', 'Ajouter…', 'Añadir…', 'Добавить…', '添加…');
K('cPickItems', 'Add files and folders', 'Dosya ve klasör ekle', 'Dateien und Ordner hinzufügen', 'Ajouter des fichiers et dossiers', 'Añadir archivos y carpetas', 'Добавить файлы и папки', '添加文件和文件夹');
K('cNone', 'No items added yet.', 'Henüz öğe eklenmedi.', 'Noch keine Elemente.', 'Aucun élément pour l’instant.', 'Aún no hay elementos.', 'Элементы не добавлены.', '尚未添加项目。');
K('cRemove', 'Remove', 'Kaldır', 'Entfernen', 'Retirer', 'Quitar', 'Убрать', '移除');
K('cArchive', 'Archive', 'Arşiv', 'Archiv', 'Archive', 'Archivo', 'Архив', '压缩包');
K('cFolderBtn', 'Folder…', 'Klasör…', 'Ordner …', 'Dossier…', 'Carpeta…', 'Папка…', '文件夹…');
K('cPickOut', 'Folder to save the archive in', 'Arşivin kaydedileceği klasör', 'Speicherordner für das Archiv', 'Dossier d’enregistrement', 'Carpeta donde guardar', 'Папка для сохранения архива', '压缩包保存位置');
K('cFormat', 'Archive format', 'Arşiv biçimi', 'Archivformat', 'Format de l’archive', 'Formato del archivo', 'Формат архива', '压缩格式');
K('cLevel', 'Compression level', 'Sıkıştırma düzeyi', 'Komprimierungsstufe', 'Niveau de compression', 'Nivel de compresión', 'Уровень сжатия', '压缩等级');
K('cMethod', 'Compression method', 'Sıkıştırma yöntemi', 'Komprimierungsmethode', 'Méthode de compression', 'Método de compresión', 'Метод сжатия', '压缩方法');
K('cDict', 'Dictionary size', 'Sözlük boyutu', 'Wörterbuchgröße', 'Taille du dictionnaire', 'Tamaño del diccionario', 'Размер словаря', '字典大小');
K('cWord', 'Word size', 'Kelime boyutu', 'Wortgröße', 'Taille des mots', 'Tamaño de palabra', 'Размер слова', '单词大小');
K('cSolid', 'Solid block size', 'Solid blok boyutu', 'Größe des soliden Blocks', 'Taille du bloc solide', 'Tamaño de bloque sólido', 'Размер непрерывного блока', '固实数据大小');
K('cThreads', 'Number of CPU threads', 'CPU iş parçacığı sayısı', 'Anzahl CPU-Threads', 'Nombre de threads CPU', 'Número de hilos de CPU', 'Число потоков ЦП', 'CPU 线程数');
K('cMem', 'Memory usage for compressing', 'Sıkıştırma için bellek sınırı', 'Speicher für Komprimierung', 'Mémoire pour la compression', 'Memoria para comprimir', 'Память для сжатия', '压缩内存用量');
K('cVol', 'Split to volumes, bytes', 'Birimlere böl (bayt)', 'In Teilarchive aufteilen (Bytes)', 'Diviser en volumes (octets)', 'Dividir en volúmenes (bytes)', 'Разбить на тома (байт)', '分卷大小（字节）');
K('cParams', 'Parameters', 'Parametreler', 'Parameter', 'Paramètres', 'Parámetros', 'Параметры', '参数');
K('cUpdate', 'Update mode', 'Güncelleme kipi', 'Aktualisierungsmodus', 'Mode de mise à jour', 'Modo de actualización', 'Режим обновления', '更新方式');
K('cPath', 'Path mode', 'Yol kipi', 'Pfadmodus', 'Mode des chemins', 'Modo de rutas', 'Режим путей', '路径模式');
K('cOpts', 'Options', 'Seçenekler', 'Optionen', 'Options', 'Opciones', 'Параметры', '选项');
K('cSfx', 'Create SFX archive', 'Kendi kendine açılan (SFX) arşiv oluştur', 'SFX-Archiv erstellen', 'Créer une archive SFX', 'Crear archivo SFX', 'Создать SFX-архив', '创建自解压压缩包');
K('cShared', 'Compress shared files', 'Paylaşılan dosyaları sıkıştır', 'Geöffnete Dateien komprimieren', 'Compresser les fichiers partagés', 'Comprimir archivos compartidos', 'Сжимать открытые файлы', '压缩共享文件');
K('cDel', 'Delete files after compression', 'Sıkıştırdıktan sonra dosyaları sil', 'Dateien nach Komprimierung löschen', 'Supprimer les fichiers après compression', 'Eliminar archivos tras comprimir', 'Удалить файлы после сжатия', '压缩后删除原文件');
K('cEnc', 'Encryption', 'Şifreleme', 'Verschlüsselung', 'Chiffrement', 'Cifrado', 'Шифрование', '加密');
K('cPw', 'Enter password', 'Parola girin', 'Passwort eingeben', 'Saisir le mot de passe', 'Introduzca la contraseña', 'Введите пароль', '输入密码');
K('cPw2', 'Re-enter password', 'Parolayı tekrar girin', 'Passwort wiederholen', 'Ressaisir le mot de passe', 'Repita la contraseña', 'Повторите пароль', '再次输入密码');
K('cShowPw', 'Show password', 'Parolayı göster', 'Passwort anzeigen', 'Afficher le mot de passe', 'Mostrar contraseña', 'Показать пароль', '显示密码');
K('cEncMethod', 'Encryption method', 'Şifreleme yöntemi', 'Verschlüsselungsmethode', 'Méthode de chiffrement', 'Método de cifrado', 'Метод шифрования', '加密方法');
K('cEncNames', 'Encrypt file names', 'Dosya adlarını şifrele', 'Dateinamen verschlüsseln', 'Chiffrer les noms de fichiers', 'Cifrar nombres de archivo', 'Шифровать имена файлов', '加密文件名');
K('cAuto', 'Auto', 'Otomatik', 'Automatisch', 'Auto', 'Auto', 'Авто', '自动');
K('lvl0', 'Store', 'Depola', 'Speichern', 'Stocker', 'Almacenar', 'Без сжатия', '仅存储');
K('lvl1', 'Fastest', 'En hızlı', 'Am schnellsten', 'Le plus rapide', 'Más rápido', 'Скоростной', '极速压缩');
K('lvl3', 'Fast', 'Hızlı', 'Schnell', 'Rapide', 'Rápido', 'Быстрый', '快速压缩');
K('lvl5', 'Normal', 'Normal', 'Normal', 'Normal', 'Normal', 'Нормальный', '标准');
K('lvl7', 'Maximum', 'Maksimum', 'Maximum', 'Maximum', 'Máximo', 'Максимальный', '最大压缩');
K('lvl9', 'Ultra', 'Ultra', 'Ultra', 'Ultra', 'Ultra', 'Ультра', '极限压缩');
K('solidOff', 'Non-solid', 'Solid değil', 'Nicht solide', 'Non solide', 'No sólido', 'Не непрерывный', '非固实');
K('solidOn', 'Solid', 'Solid (tek blok)', 'Solide', 'Solide', 'Sólido', 'Непрерывный', '固实');
K('uAdd', 'Add and replace files', 'Ekle ve değiştir', 'Hinzufügen und ersetzen', 'Ajouter et remplacer', 'Añadir y reemplazar', 'Добавить с заменой', '添加并替换文件');
K('uUpdate', 'Update and add files', 'Güncelle ve ekle', 'Aktualisieren und hinzufügen', 'Mettre à jour et ajouter', 'Actualizar y añadir', 'Обновить и добавить', '更新并添加文件');
K('uFresh', 'Freshen existing files', 'Mevcut dosyaları tazele', 'Vorhandene auffrischen', 'Actualiser les fichiers existants', 'Refrescar existentes', 'Обновить существующие', '仅更新现有文件');
K('uSync', 'Synchronize files', 'Dosyaları eşitle', 'Dateien synchronisieren', 'Synchroniser les fichiers', 'Sincronizar archivos', 'Синхронизировать', '同步文件');
K('pmRel', 'Relative pathnames', 'Göreli yollar', 'Relative Pfade', 'Chemins relatifs', 'Rutas relativas', 'Относительные пути', '相对路径');
K('pmFull', 'Full pathnames', 'Tam yollar', 'Vollständige Pfade', 'Chemins complets', 'Rutas completas', 'Полные пути', '完整路径');
K('pmAbs', 'Absolute pathnames', 'Mutlak yollar', 'Absolute Pfade', 'Chemins absolus', 'Rutas absolutas', 'Абсолютные пути', '绝对路径');
K('cSingleWarn', '{0} compresses a single file only. For folders or several files choose tar.gz / tar.bz2 / tar.xz (or 7z / zip).',
  '{0} yalnızca tek dosya sıkıştırır. Klasör veya birden fazla dosya için tar.gz / tar.bz2 / tar.xz (veya 7z / zip) seçin.',
  '{0} komprimiert nur eine einzelne Datei. Für Ordner oder mehrere Dateien wählen Sie tar.gz / tar.bz2 / tar.xz (oder 7z / zip).',
  '{0} ne compresse qu’un seul fichier. Pour des dossiers ou plusieurs fichiers, choisissez tar.gz / tar.bz2 / tar.xz (ou 7z / zip).',
  '{0} solo comprime un archivo. Para carpetas o varios archivos elija tar.gz / tar.bz2 / tar.xz (o 7z / zip).',
  '{0} сжимает только один файл. Для папок и нескольких файлов выберите tar.gz / tar.bz2 / tar.xz (или 7z / zip).',
  '{0} 只能压缩单个文件。压缩文件夹或多个文件请选择 tar.gz / tar.bz2 / tar.xz（或 7z / zip）。');
K('cPwMismatch', 'Passwords do not match.', 'Parolalar eşleşmiyor.', 'Passwörter stimmen nicht überein.', 'Les mots de passe ne correspondent pas.', 'Las contraseñas no coinciden.', 'Пароли не совпадают.', '两次输入的密码不一致。');
K('cNoItems', 'You have not added any files or folders.', 'Eklenecek dosya veya klasör seçmediniz.', 'Sie haben keine Dateien oder Ordner hinzugefügt.', 'Vous n’avez ajouté aucun fichier ni dossier.', 'No ha añadido archivos ni carpetas.', 'Вы не добавили файлы или папки.', '尚未添加任何文件或文件夹。');
K('cDelConfirm', 'The source files will be deleted after they are archived. Continue?', 'Kaynak dosyalar arşivlendikten sonra silinecek. Devam edilsin mi?', 'Die Quelldateien werden nach dem Archivieren gelöscht. Fortfahren?', 'Les fichiers source seront supprimés après archivage. Continuer ?', 'Los archivos de origen se eliminarán tras archivarlos. ¿Continuar?', 'Исходные файлы будут удалены после архивации. Продолжить?', '归档后将删除源文件。要继续吗？');
K('cExists', '{0}\n\nThis file already exists.', '{0}\n\nBu dosya zaten var.', '{0}\n\nDiese Datei existiert bereits.', '{0}\n\nCe fichier existe déjà.', '{0}\n\nEste archivo ya existe.', '{0}\n\nЭтот файл уже существует.', '{0}\n\n该文件已存在。');
K('cOverwrite', 'Overwrite', 'Üzerine yaz', 'Überschreiben', 'Écraser', 'Sobrescribir', 'Перезаписать', '覆盖');
K('cAddTo', 'Add to existing archive', 'Mevcut arşive ekle', 'Zum vorhandenen Archiv hinzufügen', 'Ajouter à l’archive existante', 'Añadir al archivo existente', 'Добавить в существующий архив', '添加到现有压缩包');
K('cCreated', 'Archive created: {0}', 'Arşiv oluşturuldu: {0}', 'Archiv erstellt: {0}', 'Archive créée : {0}', 'Archivo creado: {0}', 'Архив создан: {0}', '压缩包已创建：{0}');

/* --- server error codes --- */
K('e.E_NO_7Z', '7-Zip not found.', '7-Zip bulunamadı.', '7-Zip nicht gefunden.', '7-Zip introuvable.', '7-Zip no encontrado.', '7-Zip не найден.', '未找到 7-Zip。');
K('e.E_NOT_FOUND', 'Not found: {0}', 'Bulunamadı: {0}', 'Nicht gefunden: {0}', 'Introuvable : {0}', 'No encontrado: {0}', 'Не найдено: {0}', '未找到：{0}');
K('e.E_ARC_NOT_FOUND', 'Archive not found.', 'Arşiv bulunamadı.', 'Archiv nicht gefunden.', 'Archive introuvable.', 'Archivo no encontrado.', 'Архив не найден.', '未找到压缩包。');
K('e.E_DEST_EMPTY', 'Destination folder cannot be empty.', 'Hedef klasör boş olamaz.', 'Zielordner darf nicht leer sein.', 'Le dossier de destination est vide.', 'La carpeta de destino no puede estar vacía.', 'Папка назначения не указана.', '目标文件夹不能为空。');
K('e.E_BAD_REQUEST', 'Invalid request.', 'Geçersiz istek.', 'Ungültige Anfrage.', 'Requête invalide.', 'Solicitud no válida.', 'Некорректный запрос.', '无效的请求。');
K('e.E_NO_SOURCES', 'No items to add.', 'Eklenecek öğe yok.', 'Keine Elemente zum Hinzufügen.', 'Aucun élément à ajouter.', 'No hay elementos que añadir.', 'Нет элементов для добавления.', '没有要添加的项目。');
K('e.E_BAD_FORMAT', 'Unsupported format.', 'Desteklenmeyen biçim.', 'Nicht unterstütztes Format.', 'Format non pris en charge.', 'Formato no compatible.', 'Неподдерживаемый формат.', '不支持的格式。');
K('e.E_OUT_EMPTY', 'Archive file name is empty.', 'Arşiv dosya adı boş.', 'Archivdateiname ist leer.', 'Le nom du fichier d’archive est vide.', 'El nombre del archivo está vacío.', 'Не указано имя архива.', '压缩包文件名为空。');
K('e.E_OUT_DIR', 'Destination folder does not exist: {0}', 'Hedef klasör yok: {0}', 'Zielordner existiert nicht: {0}', 'Le dossier de destination n’existe pas : {0}', 'La carpeta de destino no existe: {0}', 'Папка назначения не существует: {0}', '目标文件夹不存在：{0}');
K('e.E_RM_FAIL', 'Could not delete the existing file: {0}', 'Mevcut dosya silinemedi: {0}', 'Vorhandene Datei konnte nicht gelöscht werden: {0}', 'Impossible de supprimer le fichier existant : {0}', 'No se pudo eliminar el archivo existente: {0}', 'Не удалось удалить существующий файл: {0}', '无法删除现有文件：{0}');
K('e.E_NO_JOB', 'Job not found.', 'İş bulunamadı.', 'Auftrag nicht gefunden.', 'Tâche introuvable.', 'Tarea no encontrada.', 'Задание не найдено.', '未找到任务。');
K('e.E_OPEN_FAIL', 'Could not open the folder.', 'Klasör açılamadı.', 'Ordner konnte nicht geöffnet werden.', 'Impossible d’ouvrir le dossier.', 'No se pudo abrir la carpeta.', 'Не удалось открыть папку.', '无法打开文件夹。');
K('e.E_NO_FILES', 'No files selected.', 'Dosya seçilmedi.', 'Keine Dateien ausgewählt.', 'Aucun fichier sélectionné.', 'No se seleccionaron archivos.', 'Файлы не выбраны.', '未选择文件。');
K('e.E_NOT_READY', 'Result is not ready.', 'Sonuç hazır değil.', 'Ergebnis ist noch nicht bereit.', 'Résultat non prêt.', 'El resultado no está listo.', 'Результат ещё не готов.', '结果尚未就绪。');
K('e.E_READ_DIR', 'Cannot read folder: {0}', 'Klasör okunamadı: {0}', 'Ordner kann nicht gelesen werden: {0}', 'Impossible de lire le dossier : {0}', 'No se puede leer la carpeta: {0}', 'Не удалось прочитать папку: {0}', '无法读取文件夹：{0}');
K('e.E_BAD_DIRNAME', 'Invalid folder name.', 'Geçersiz klasör adı.', 'Ungültiger Ordnername.', 'Nom de dossier invalide.', 'Nombre de carpeta no válido.', 'Недопустимое имя папки.', '无效的文件夹名称。');
K('e.E_MKDIR', 'Could not create folder: {0}', 'Klasör oluşturulamadı: {0}', 'Ordner konnte nicht erstellt werden: {0}', 'Impossible de créer le dossier : {0}', 'No se pudo crear la carpeta: {0}', 'Не удалось создать папку: {0}', '无法创建文件夹：{0}');
K('e.E_SINGLE_FILE', '{0} compresses a single file only. Choose tar.gz / tar.bz2 / tar.xz for folders or several files.', '{0} yalnızca tek dosya sıkıştırır. Klasör veya birden fazla dosya için tar.gz / tar.bz2 / tar.xz seçin.', '{0} komprimiert nur eine Datei. Wählen Sie für Ordner oder mehrere Dateien tar.gz / tar.bz2 / tar.xz.', '{0} ne compresse qu’un seul fichier. Choisissez tar.gz / tar.bz2 / tar.xz pour des dossiers ou plusieurs fichiers.', '{0} solo comprime un archivo. Elija tar.gz / tar.bz2 / tar.xz para carpetas o varios archivos.', '{0} сжимает только один файл. Для папок и нескольких файлов выберите tar.gz / tar.bz2 / tar.xz.', '{0} 只能压缩单个文件。压缩文件夹或多个文件请选择 tar.gz / tar.bz2 / tar.xz。');
K('e.E_NO_ENC', 'This format does not support encryption.', 'Bu biçim şifrelemeyi desteklemiyor.', 'Dieses Format unterstützt keine Verschlüsselung.', 'Ce format ne prend pas en charge le chiffrement.', 'Este formato no admite cifrado.', 'Этот формат не поддерживает шифрование.', '此格式不支持加密。');
K('e.E_PARAMS', 'Invalid parameter: {0} (must start with “-”).', 'Geçersiz parametre: {0} (“-” ile başlamalı).', 'Ungültiger Parameter: {0} (muss mit „-“ beginnen).', 'Paramètre invalide : {0} (doit commencer par « - »).', 'Parámetro no válido: {0} (debe empezar por «-»).', 'Недопустимый параметр: {0} (должен начинаться с «-»).', '无效参数：{0}（必须以“-”开头）。');
K('e.E_7Z', '{0}', '{0}', '{0}', '{0}', '{0}', '{0}', '{0}');

/* ===================== app ===================== */
const T = new URLSearchParams(location.search).get('t') || '';
const $ = (s, r) => (r || document).querySelector(s);

function h(tag, attrs, ...kids){
  const el = document.createElement(tag);
  if (attrs) for (const [k, v] of Object.entries(attrs)){
    if (k === 'class') el.className = v;
    else if (k.startsWith('on')) el.addEventListener(k.slice(2), v);
    else if (v === true) el.setAttribute(k, '');
    else if (v !== false && v != null) el.setAttribute(k, v);
  }
  for (const k of kids.flat()){
    if (k == null || k === false) continue;
    el.append(k.nodeType ? k : document.createTextNode(String(k)));
  }
  return el;
}
const esc = s => String(s).replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
function lsGet(k){ try { return localStorage.getItem(k); } catch(e){ return null; } }
function lsSet(k, v){ try { localStorage.setItem(k, v); } catch(e){} }
function fmtSize(n){
  if (n == null || n === '') return '';
  n = Number(n);
  for (const u of ['B','KB','MB','GB','TB']){
    if (n < 1024 || u === 'TB') return u === 'B' ? n.toFixed(0) + ' B' : n.toFixed(1) + ' ' + u;
    n /= 1024;
  }
}

/* ---------- language + theme ---------- */
let LANG = 'en';
function detectLang(){
  const s = lsGet('lang'); if (s && D[s]) return s;
  for (const l of (navigator.languages && navigator.languages.length ? navigator.languages : [navigator.language || 'en'])){
    const c = String(l).toLowerCase().slice(0, 2); if (D[c]) return c;
  }
  return 'en';
}
function t(key, ...a){
  let s = D[LANG] && D[LANG][key]; if (s == null) s = D.en[key]; if (s == null) return key;
  return s.replace(/\{(\d+)\}/g, (_, i) => a[i] == null ? '' : a[i]);
}
function tErr(code, arg){
  const k = 'e.' + code;
  return D.en[k] != null ? t(k, arg) : code + (arg ? ': ' + arg : '');
}
function tProp(k){ return (D[LANG] && D[LANG]['k.' + k]) || D.en['k.' + k] || k; }
let THEME = 'auto';
function applyTheme(v){
  THEME = ['light', 'dark', 'oled'].includes(v) ? v : 'auto';
  if (THEME === 'auto') document.documentElement.removeAttribute('data-theme');
  else document.documentElement.setAttribute('data-theme', THEME);
}

/* ---------- path helpers (Windows + POSIX) ---------- */
const S = {archive:null, password:'', root:null, nodes:null, expanded:new Set(), selected:new Set(), flat:[], info:null, st:null};
const isWin = () => !!(S.info && S.info.win);
const SEP = () => (S.info && S.info.sep) || '/';
function join(dir, name){
  if (!dir) return name;
  return (isWin() ? /[\\/]$/ : /\/$/).test(dir) ? dir + name : dir + SEP() + name;
}
function lastSep(q){ return isWin() ? Math.max(q.lastIndexOf('/'), q.lastIndexOf('\\')) : q.lastIndexOf('/'); }
function dirname(p){
  const q = p.replace(isWin() ? /[\\/]+$/ : /\/+$/, '');
  const i = lastSep(q);
  const fix = d => isWin() && /^[A-Za-z]:$/.test(d) ? d + SEP() : d;
  if (i < 0) return fix(q) === q ? '' : fix(q);
  if (i === 0) return SEP();
  return fix(q.slice(0, i));
}
function basename(p){ return p.replace(isWin() ? /[\\/]+$/ : /\/+$/, '').split(isWin() ? /[\\/]/ : '/').pop(); }

async function api(path, body){
  const url = path + (path.includes('?') ? '&' : '?') + 't=' + encodeURIComponent(T);
  const opt = body === undefined ? {} : {method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify(body)};
  const r = await fetch(url, opt);
  let j;
  try { j = await r.json(); } catch(e){ throw new Error(t('noServer') + ' (' + r.status + ')'); }
  if (j && typeof j.error === 'string' && j.ecode){ j.code = j.ecode; j.error = tErr(j.ecode, j.arg); }
  return j;
}
function status(key, ...args){ S.st = [key, args]; $('#status').textContent = t(key, ...args); }

/* ---------- modal system ---------- */
const stack = [];
function openModal(title, bodyEl, buttons, opts){
  opts = Object.assign({width:520, esc:true}, opts || {});
  const ov = h('div', {class:'ov'});
  const box = h('div', {class:'modal', style:'width:' + opts.width + 'px'});
  const foot = h('div', {class:'mf'});
  let resolve; const p = new Promise(r => resolve = r);
  const m = {el:box, promise:p, opts, buttons:[], primary:null,
    close(v){ const i = stack.indexOf(m); if (i >= 0) stack.splice(i, 1); ov.remove(); resolve(v); },
    onEnter(input){ input.addEventListener('keydown', e => { if (e.key === 'Enter' && m.primary){ e.preventDefault(); m.primary.el.click(); } }); }
  };
  for (const b of buttons){
    const el = h('button', {class:'btn ' + (b.kind || ''), onclick: async () => {
      if (b.action){
        const r = await b.action(m);
        if (r === false) return;
        if (r !== undefined && r !== true){ m.close(r); return; }
      }
      m.close(b.value);
    }}, b.label);
    b.el = el; m.buttons.push(b); foot.append(el);
    if (b.kind === 'primary' && !m.primary) m.primary = b;
  }
  const head = h('div', {class:'mh'}, title); m.head = head;
  box.append(head, h('div', {class:'mb'}, bodyEl), foot);
  ov.append(box); $('#modals').append(ov); stack.push(m);
  setTimeout(() => { const f = box.querySelector('[autofocus]'); if (f) f.focus(); }, 0);
  return m;
}
document.addEventListener('keydown', e => {
  if (e.key !== 'Escape' || !stack.length) return;
  const m = stack[stack.length - 1];
  if (m.opts.esc) m.close(null);
});
function alertBox(msg, title){ return openModal(title || '7Zip GUI', h('div', {class:'msg'}, msg), [{label:t('ok'), kind:'primary', value:true}]).promise; }
function choose(msg, buttons, title){ return openModal(title || '7Zip GUI', h('div', {class:'msg'}, msg), buttons).promise; }
function askPassword(msg){
  const inp = h('input', {type:'password', class:'in', autofocus:true});
  const m = openModal(t('pwTitle'), h('div', null, h('div', {class:'msg', style:'margin-bottom:10px'}, msg), inp),
    [{label:t('cancel'), value:null}, {label:t('ok'), kind:'primary', action: () => inp.value || false}]);
  m.onEnter(inp);
  return m.promise;
}

/* ---------- file picker ---------- */
/* mode: 'file' -> path string, 'dir' -> path string, 'multi' -> [{p, d}] */
function pickPath(o){
  return new Promise(async resolve => {
    let cur = o.start || lsGet('lastDir') || '', hidden = false, selFile = null, par = null;
    const selected = new Map();
    const addr = h('input', {class:'in'});
    const list = h('div', {class:'fl'});
    const cnt = h('span', {class:'dim'});
    const upBtn = h('button', {class:'btn', title:t('pickUp'), onclick: () => { if (par != null) load(par); }}, '↑');
    async function load(path){
      let r; try { r = await api('/api/ls?path=' + encodeURIComponent(path) + '&hidden=' + (hidden ? 1 : 0)); }
      catch(e){ await alertBox(e.message); return; }
      if (r.error){ await alertBox(r.error); return; }
      cur = r.path; par = r.parent; addr.value = cur; if (cur) lsSet('lastDir', cur); selFile = null;
      upBtn.disabled = par == null;
      const rows = [];
      if (par != null) rows.push(h('div', {class:'fr', onclick: () => load(par)}, h('span', null, '📁'), h('span', {class:'n'}, '..')));
      for (const e of r.entries){
        const full = e.f || join(cur, e.n);
        const row = h('div', {class:'fr'});
        if (o.mode === 'multi'){
          const cb = h('input', {type:'checkbox'}); cb.checked = selected.has(full);
          cb.addEventListener('click', ev => { ev.stopPropagation(); cb.checked ? selected.set(full, e.d) : selected.delete(full); upd(); });
          row.append(cb);
        }
        row.append(h('span', null, e.d ? '📁' : '📄'), h('span', {class:'n', title:e.n}, e.n),
          h('span', {class:'m'}, e.d ? '' : fmtSize(e.s)));
        row.addEventListener('click', () => {
          if (e.d){ load(full); return; }
          if (o.mode === 'multi'){ const cb = row.querySelector('input'); cb.checked = !cb.checked; cb.checked ? selected.set(full, false) : selected.delete(full); upd(); }
          else if (o.mode === 'file'){ list.querySelectorAll('.fr.on').forEach(x => x.classList.remove('on')); row.classList.add('on'); selFile = full; }
        });
        if (o.mode === 'file' && !e.d) row.addEventListener('dblclick', () => m.close(full));
        rows.push(row);
      }
      if (!r.entries.length) rows.push(h('div', {class:'mut'}, t('emptyFolder')));
      list.replaceChildren(...rows); list.scrollTop = 0;
    }
    function upd(){ cnt.textContent = selected.size ? t('selCount', selected.size) : ''; }
    const homeBtn = h('button', {class:'btn', title:t('pickHome'), onclick: () => load('~')}, '🏠');
    const drvBtn = isWin() ? h('button', {class:'btn', title:t('drives'), onclick: () => load('@drives')}, '💽') : null;
    const body = h('div', null,
      h('div', {class:'row', style:'margin-bottom:0'}, upBtn, homeBtn, drvBtn, addr),
      h('div', {class:'row', style:'margin:6px 0 0'},
        h('label', {style:'min-width:0;display:flex;gap:6px;align-items:center'},
          h('input', {type:'checkbox', onchange: e => { hidden = e.target.checked; load(cur); }}), t('showHidden')),
        h('span', {style:'flex:1'}), cnt),
      list);
    addr.addEventListener('keydown', e => { if (e.key === 'Enter') load(addr.value || '~'); });
    const btns = [{label:t('cancel'), value:null}];
    if (o.mode === 'dir'){
      btns.unshift({label:t('newFolder'), action: async () => {
        if (!cur) return false;
        const inp = h('input', {class:'in', autofocus:true});
        const mm = openModal(t('newFolderTitle'), inp, [{label:t('cancel'), value:null}, {label:t('create'), kind:'primary', action: () => inp.value.trim() || false}]);
        mm.onEnter(inp);
        const name = await mm.promise;
        if (name){
          const r = await api('/api/mkdir', {path:cur, name});
          if (r.error) await alertBox(r.error); else await load(r.path);
        }
        return false;
      }});
      btns.push({label:t('selectFolder'), kind:'primary', action: () => cur || false});
    } else if (o.mode === 'file'){
      btns.push({label:t('btnOpen'), kind:'primary', action: () => selFile || false});
    } else {
      btns.push({label:t('btnAdd'), kind:'primary', action: () => selected.size ? [...selected].map(([p, d]) => ({p, d})) : false});
    }
    const m = openModal(o.title, body, btns, {width:640});
    m.promise.then(resolve);
    load(cur || '~');
  });
}

/* ---------- formatting helpers ---------- */
function fmtHMS(s){
  if (s == null || !isFinite(s)) return '—';
  s = Math.round(s); const p = n => String(n).padStart(2, '0');
  return p(Math.floor(s / 3600)) + ':' + p(Math.floor(s / 60) % 60) + ':' + p(s % 60);
}
function fmtMB(n){
  if (n == null) return '—';
  return n >= 1048576 ? Math.round(n / 1048576) + ' MB' : Math.round(n / 1024) + ' KB';
}
function grp(n){ return n == null || n === '' ? '' : String(n).replace(/\B(?=(\d{3})+(?!\d))/g, ' '); }
function baseTitle(){ return S.archive ? basename(S.archive) + ' - 7Zip GUI' : '7Zip GUI'; }
async function copyText(s){
  try { await navigator.clipboard.writeText(s); return true; }
  catch(e){
    const ta = h('textarea', {style:'position:fixed;opacity:0'}); ta.value = s; document.body.append(ta); ta.select();
    let ok = false; try { ok = document.execCommand('copy'); } catch(_){}
    ta.remove(); return ok;
  }
}

/* ---------- progress window (Windows 7-Zip layout) ---------- */
function runProgress(title, jobId, opts){
  const verb = (opts && opts.verb) || 'vWork';
  return new Promise(resolve => {
    let finished = false, result = null, since = 0, paused = false, bg = false;
    const V = {};
    const field = (label, key) => { V[key] = h('span', {class:'v'}, '—'); return [h('span', null, t(label)), V[key]]; };
    const left = h('div', {class:'kv'}, field('pElapsed', 'el'), field('pRemain', 'rem'), field('pFiles', 'files'));
    const right = h('div', {class:'kv'}, field('pTotal', 'total'), field('pSpeed', 'speed'), field('pProc', 'proc'),
      field('pComp', 'comp'), field('pRatio', 'ratio'));
    const curEl = h('div', {class:'curfile'});
    const fill = h('div', {class:'fill'});
    const bar = h('div', {class:'bar ind'}, fill);
    const note = h('div', {class:'plbl'});
    const log = h('pre', {class:'log', style:'display:none'});
    const body = h('div', null, h('div', {class:'pgrid'}, left, right), h('div', {class:'verb'}, t(verb)), curEl, bar, note, log);
    const m = openModal(title, body, [
      {label:t('bBg'), action: async () => {
        try {
          const r = await api('/api/background', {job:jobId, on:!bg});
          bg = !!r.background;
          note.textContent = r.applied === false ? t('prioFail') : '';
        } catch(e){}
        bgBtn.textContent = bg ? t('bFg') : t('bBg');
        return false;
      }},
      {label:t('bPause'), action: async () => {
        try { const r = await api('/api/pause', {job:jobId, paused:!paused}); paused = !!r.paused; } catch(e){}
        pauseBtn.textContent = paused ? t('bResume') : t('bPause');
        return false;
      }},
      {label:t('cancel'), action: async () => {
        if (finished) return true;
        try { await api('/api/cancel', {job:jobId}); } catch(e){}
        return false;
      }}
    ], {esc:false, width:620});
    const bgBtn = m.buttons[0].el, pauseBtn = m.buttons[1].el, cancelBtn = m.buttons[2].el;
    m.promise.then(() => { document.title = baseTitle(); resolve(result || {rc:-1, cancelled:true}); });
    function end(){
      finished = true; bgBtn.style.display = 'none'; pauseBtn.style.display = 'none';
      cancelBtn.textContent = t('close'); m.opts.esc = true; document.title = baseTitle();
    }
    function fail(msg){ note.textContent = msg; note.classList.add('err'); end(); }
    async function poll(){
      let r;
      try { r = await api('/api/job?id=' + jobId + '&since=' + since); }
      catch(e){ fail(t('noServer')); return; }
      if (r.error){ fail(r.error); return; }
      since = r.next;
      if (r.lines.length) log.textContent += r.lines.join('\n') + '\n';
      paused = r.paused; pauseBtn.textContent = paused ? t('bResume') : t('bPause');
      V.el.textContent = fmtHMS(r.el); V.rem.textContent = fmtHMS(r.rem);
      V.files.textContent = r.files == null ? '—' : (r.files_total ? r.files + ' / ' + r.files_total : String(r.files));
      V.total.textContent = fmtMB(r.total);
      V.speed.textContent = r.speed == null ? '—' : fmtMB(r.speed) + '/s';
      V.proc.textContent = fmtMB(r.processed); V.comp.textContent = fmtMB(r.comp);
      V.ratio.textContent = r.ratio == null ? '—' : Math.round(r.ratio) + '%';
      curEl.textContent = r.cur || '';
      if (r.pct != null){ bar.classList.remove('ind'); fill.style.width = r.pct + '%'; }
      const head = (r.pct != null ? r.pct + '% ' : '') + (paused ? t('paused') : t(verb) + '…');
      m.head.textContent = head; document.title = head;
      if (r.done){
        result = {rc:r.rc, cancelled:r.cancelled};
        if (r.cancelled || r.rc === 0){ finished = true; m.close(); return; }
        end();
        bar.classList.remove('ind'); fill.style.width = '100%';
        m.head.textContent = r.rc === 1 ? t('doneWarn') : t('doneErr');
        note.textContent = r.rc === 1 ? t('noteWarn') : t('noteErr', r.rc);
        note.classList.add(r.rc === 1 ? 'warn' : 'err');
        log.style.display = 'block'; log.scrollTop = log.scrollHeight;
        return;
      }
      setTimeout(poll, 250);
    }
    poll();
  });
}
async function launch(title, endpoint, body, opts){
  let res;
  try { res = await api(endpoint, body); } catch(e){ await alertBox(e.message); return null; }
  if (res.error){ await alertBox(res.error); return null; }
  return runProgress(title, res.job, opts);
}

/* ---------- archive tree ---------- */
function buildTree(entries){
  const nodes = new Map(); const root = {key:'', dir:true, children:[]}; nodes.set('', root);
  function ensureDir(key){
    if (nodes.has(key)) return nodes.get(key);
    const i = key.lastIndexOf('/');
    const parent = ensureDir(i < 0 ? '' : key.slice(0, i));
    const n = {key, name:key.slice(i + 1), dir:true, orig:key, children:[], parent, size:0, mod:'', attr:'', enc:false};
    parent.children.push(n); nodes.set(key, n); return n;
  }
  for (const e of entries){
    const key = (e.k || e.p).replace(/^(\.\/|\/)+/, '').replace(/\/+$/, '');
    if (!key) continue;
    if (e.d){ const n = ensureDir(key); n.orig = e.p.replace(/[\\/]+$/, ''); n.mod = e.m; n.attr = e.a; continue; }
    const i = key.lastIndexOf('/');
    const parent = ensureDir(i < 0 ? '' : key.slice(0, i));
    const n = {key, name:key.slice(i + 1), dir:false, orig:e.p, parent, size:e.s, packed:e.c, mod:e.m, attr:e.a, enc:e.e};
    parent.children.push(n); nodes.set(key, n);
  }
  (function sort(n){
    n.children.sort((a, b) => (a.dir === b.dir ? 0 : a.dir ? -1 : 1) || a.name.localeCompare(b.name));
    let sum = 0, cnt = 0;
    for (const c of n.children){ if (c.dir){ sort(c); sum += c.total; cnt += c.nfiles; } else { sum += c.size || 0; cnt++; } }
    if (n.dir && n.key !== '') n.size = sum;
    n.total = sum; n.nfiles = cnt;
  })(root);
  return {root, nodes};
}
function flatten(node, depth, out){
  for (const c of node.children){
    out.push([c, depth]);
    if (c.dir && S.expanded.has(c.key)) flatten(c, depth + 1, out);
  }
}
function rowHtml(n, d, i){
  const sel = S.selected.has(n.key);
  const tw = '<span class="tw">' + (n.dir ? (S.expanded.has(n.key) ? '▾' : '▸') : '') + '</span>';
  return '<tr data-i="' + i + '" class="' + (sel ? 'sel' : '') + '"><td class="c"><input type="checkbox" ' + (sel ? 'checked' : '') + '></td>' +
    '<td class="nm" title="' + esc(n.key) + '" style="padding-left:' + (8 + d * 18) + 'px">' + tw + '<span class="ic">' + (n.dir ? '📁' : '📄') + '</span>' + esc(n.name) + '</td>' +
    '<td class="r ' + (n.dir ? 'dim' : '') + '">' + fmtSize(n.size) + '</td><td class="r">' + (n.dir ? '' : fmtSize(n.packed)) + '</td>' +
    '<td>' + esc(n.mod || '') + '</td><td>' + esc(n.attr || '') + '</td><td>' + (n.enc ? t('yes') : '') + '</td></tr>';
}
function render(){
  const has = !!S.archive;
  $('#tbl').style.display = has ? '' : 'none';
  $('#empty').style.display = has ? 'none' : 'flex';
  $('#title').textContent = has ? S.archive : '';
  document.title = baseTitle();
  if (!has){ $('#rows').innerHTML = ''; updSel(); return; }
  S.flat = []; flatten(S.root, 0, S.flat);
  $('#rows').innerHTML = S.flat.map(([n, d], i) => rowHtml(n, d, i)).join('');
  updSel();
}
function updSel(){
  $('#selinfo').textContent = S.selected.size ? t('selCount', S.selected.size) : '';
  $('#all').checked = !!S.root && S.root.children.length > 0 && S.root.children.every(c => S.selected.has(c.key));
}
$('#rows').addEventListener('click', e => {
  const tr = e.target.closest('tr'); if (!tr) return;
  const n = S.flat[+tr.dataset.i][0];
  if (e.target.tagName === 'INPUT'){
    e.target.checked ? S.selected.add(n.key) : S.selected.delete(n.key);
    tr.classList.toggle('sel', e.target.checked); updSel(); return;
  }
  if (n.dir && e.target.closest('.nm')){
    S.expanded.has(n.key) ? S.expanded.delete(n.key) : S.expanded.add(n.key);
    render();
  }
});
$('#rows').addEventListener('dblclick', e => {
  const tr = e.target.closest('tr'); if (!tr) return;
  const n = S.flat[+tr.dataset.i][0];
  if (!n.dir) downloadFile(n);
});
$('#all').addEventListener('change', e => {
  S.root.children.forEach(c => e.target.checked ? S.selected.add(c.key) : S.selected.delete(c.key));
  render();
});
function selectedNodes(){ return [...S.selected].map(k => S.nodes.get(k)).filter(Boolean); }

/* ---------- operations ---------- */
async function openArchive(path, pw){
  pw = pw || '';
  document.body.classList.add('busy'); status('reading');
  let r;
  try { r = await api('/api/open', {path, password:pw}); }
  catch(e){ document.body.classList.remove('busy'); await alertBox(e.message); return; }
  document.body.classList.remove('busy');
  if (r.needPassword){
    const p = await askPassword(r.wrong ? t('pwWrong') : t('pwNeeded'));
    if (p) return openArchive(path, p);
    status('cancelled'); return;
  }
  if (r.error){ await alertBox(t('arcOpenFail', r.error)); status('openFailed'); return; }
  S.archive = path; S.password = pw; S.selected.clear(); S.expanded.clear();
  S.props = r.props || [];
  { let size = 0, packed = 0, files = 0, folders = 0;
    for (const e of r.entries){ if (e.d) folders++; else { files++; size += e.s || 0; } packed += e.c || 0; }
    S.stats = {size, packed, files, folders}; }
  const tr = buildTree(r.entries); S.root = tr.root; S.nodes = tr.nodes;
  const tops = S.root.children;
  if (tops.length === 1 && tops[0].dir) S.expanded.add(tops[0].key);
  render();
  const files = r.entries.filter(e => !e.d).length;
  status('statusArc', files, fmtSize(S.root.total), fmtSize(r.size));
}
async function chooseArchive(){
  const p = await pickPath({mode:'file', title:t('pickArc')});
  if (p) openArchive(p);
}
async function extractDialog(){
  if (!S.archive){ await alertBox(t('openFirst')); return; }
  const sel = selectedNodes();
  const stem = basename(S.archive).replace(/(\.tar)?\.[^.]+$/, '') || 'extracted';
  const dest = h('input', {class:'in', value: join(dirname(S.archive), stem)});
  const mk = (v, txt, on) => h('label', null, h('input', {type:'radio', name:'scope', value:v, checked:on, disabled: v === 'sel' && !sel.length}), ' ' + txt);
  const scopeAll = mk('all', t('exAll'), !sel.length), scopeSel = mk('sel', t('exSel', sel.length), !!sel.length);
  const mode = h('select', {class:'in'}, ['aoa', 'aos', 'aou', 'aot'].map((v, i) => h('option', {value:v}, t('ow' + (i + 1)))));
  const keep = h('input', {type:'checkbox', checked:true});
  const pw = h('input', {type:'password', class:'in', value:S.password});
  const body = h('div', null,
    h('div', {class:'row'}, h('label', null, t('exDest')), dest,
      h('button', {class:'btn', onclick: async () => { const d = await pickPath({mode:'dir', title:t('exPickDest'), start:dest.value}); if (d) dest.value = d; }}, t('browse'))),
    h('fieldset', {class:'grp'}, h('legend', null, t('exScope')), scopeAll, scopeSel),
    h('div', {class:'row'}, h('label', null, t('exIfExists')), mode),
    h('div', {class:'row'}, h('label', null, ''), h('label', {style:'min-width:0'}, keep, ' ' + t('exKeep'))),
    h('div', {class:'row'}, h('label', null, t('password')), pw));
  const m = openModal(t('exTitle'), body, [{label:t('cancel'), value:null}, {label:t('bExtract'), kind:'primary', action: () => {
    if (!dest.value.trim()){ dest.focus(); return false; }
    return {dest:dest.value.trim(), scope: scopeSel.querySelector('input').checked ? 'sel' : 'all', mode:mode.value, keep:keep.checked, password:pw.value};
  }}], {width:600});
  m.onEnter(dest); m.onEnter(pw);
  const o = await m.promise; if (!o) return;
  const z = o.scope === 'sel' ? selSizing() : {total:S.stats.size, packed:S.stats.packed, files:S.stats.files};
  const r = await launch(t('tExtract'), '/api/extract', {archive:S.archive, password:o.password, dest:o.dest, scope:o.scope,
    items: o.scope === 'sel' ? sel.map(n => ({p:n.orig, d:n.dir})) : [], keep:o.keep, mode:o.mode,
    total:z.total, packed:z.packed, files:z.files}, {verb:'vExtract'});
  if (r && (r.rc === 0 || r.rc === 1)){
    if (o.password) S.password = o.password;
    status('exStatus', o.dest);
    const c = await choose(t('exDone') + '\n' + o.dest, [{label:t('close'), value:'k'}, {label:t('exOpenFolder'), kind:'primary', value:'o'}]);
    if (c === 'o') api('/api/reveal', {path:o.dest});
  }
}
async function testArchive(){
  if (!S.archive){ await alertBox(t('openFirst')); return; }
  const r = await launch(t('tTest'), '/api/test', {archive:S.archive, password:S.password,
    total:S.stats.size, packed:S.stats.packed, files:S.stats.files}, {verb:'vTest'});
  if (r && r.rc === 0) await alertBox(t('testOk'));
}
async function downloadFile(n){
  const res = await api('/api/fetch', {archive:S.archive, password:S.password, path:n.orig});
  if (res.error){ await alertBox(res.error); return; }
  const r = await runProgress(t('tFetch'), res.job, {verb:'vExtract'});
  if (r && (r.rc === 0 || r.rc === 1)){
    const a = h('a', {href:'/api/file?id=' + res.job + '&t=' + encodeURIComponent(T), download:n.name});
    document.body.append(a); a.click(); a.remove();
  }
}

/* ---------- Add to Archive dialog ---------- */
const FORMATS = [['7z', '7z'], ['zip', 'zip'], ['tar', 'tar'], ['gzip', 'gzip (.gz)'], ['bzip2', 'bzip2 (.bz2)'], ['xz', 'xz'], ['wim', 'wim'],
  ['tar.gz', 'tar.gz'], ['tar.bz2', 'tar.bz2'], ['tar.xz', 'tar.xz']];
const FEXT = {'7z':'.7z', zip:'.zip', tar:'.tar', gzip:'.gz', bzip2:'.bz2', xz:'.xz', wim:'.wim', 'tar.gz':'.tar.gz', 'tar.bz2':'.tar.bz2', 'tar.xz':'.tar.xz'};
const ALLEXT = ['.tar.gz', '.tar.bz2', '.tar.xz', '.7z', '.zip', '.tar', '.gz', '.bz2', '.xz', '.wim', '.exe', '.sfx'];
const SINGLE = ['gzip', 'bzip2', 'xz'];
const FMETH = {'7z':['LZMA2', 'LZMA', 'PPMd', 'BZip2', 'Deflate', 'Deflate64', 'Copy'], zip:['Deflate', 'Deflate64', 'BZip2', 'LZMA', 'PPMd', 'Copy'],
  gzip:['Deflate'], bzip2:['BZip2'], xz:['LZMA2'], 'tar.gz':['Deflate'], 'tar.bz2':['BZip2'], 'tar.xz':['LZMA2']};
const THREADED = ['7z', 'zip', 'bzip2', 'xz', 'tar.bz2', 'tar.xz'];
const LEVELS = [[0, 'lvl0'], [1, 'lvl1'], [3, 'lvl3'], [5, 'lvl5'], [7, 'lvl7'], [9, 'lvl9']];
const SOLIDS = ['off', '1m', '2m', '4m', '8m', '16m', '32m', '64m', '128m', '256m', '512m', '1g', '2g', '4g', '8g', '16g', '32g', '64g', 'on'];
function dictOpts(m){
  if (m === 'LZMA2' || m === 'LZMA') return ['64k', '256k', '1m', '2m', '3m', '4m', '6m', '8m', '12m', '16m', '24m', '32m', '48m', '64m', '96m', '128m', '192m', '256m', '384m', '512m', '768m', '1024m', '1536m'];
  if (m === 'PPMd') return ['1m', '2m', '4m', '8m', '16m', '32m', '64m', '128m', '256m', '512m', '1024m'];
  if (m === 'BZip2') return ['100k', '200k', '300k', '400k', '500k', '600k', '700k', '800k', '900k'];
  return null;
}
function wordOpts(m){
  if (m === 'LZMA2' || m === 'LZMA') return [8, 12, 16, 24, 32, 48, 64, 96, 128, 192, 256, 273];
  if (m === 'PPMd') return [2, 3, 4, 5, 6, 8, 12, 16, 24, 32];
  if (m === 'Deflate') return [8, 12, 16, 24, 32, 48, 64, 96, 128, 192, 258];
  if (m === 'Deflate64') return [8, 12, 16, 24, 32, 48, 64, 96, 128, 192, 257];
  return null;
}
const dictLabel = v => v.replace(/k$/i, ' KB').replace(/m$/i, ' MB').replace(/g$/i, ' GB');
const opt = (v, label) => h('option', {value:v}, label == null ? v : label);

async function createDialog(initial){
  const sources = (initial || []).map(p => typeof p === 'string' ? {p, d:false} : p);
  const hw = (S.info && S.info.threads) || 1;
  const fields = [];
  const fld = (labelKey, el) => { const lb = h('label', null, t(labelKey)); fields.push([lb, el]); return h('div', {class:'cf'}, lb, el); };
  const sel = (cls) => h('select', {class:'in ' + (cls || '')});

  const list = h('div', {class:'srcs'});
  const out = h('input', {class:'in', placeholder: isWin() ? 'C:\\archive.7z' : '/path/archive.7z'});
  let outAuto = true;
  out.addEventListener('input', () => { outAuto = false; });
  const fmt = sel(); FORMATS.forEach(([v, l]) => fmt.append(opt(v, l)));
  const level = sel(); LEVELS.forEach(([v, k]) => level.append(opt(v, t(k)))); level.value = '5';
  const method = sel(), dict = sel(), word = sel(), solid = sel();
  const threads = sel(); threads.append(opt('', t('cAuto'))); for (let i = 1; i <= hw; i++) threads.append(opt(i, i));
  const memuse = sel(); memuse.append(opt('', t('cAuto'))); [10, 25, 50, 75, 100].forEach(v => memuse.append(opt(v, v + '%')));
  const vol = h('input', {class:'in', list:'vollist', placeholder:'100m', autocomplete:'off'});
  const vlist = h('datalist', {id:'vollist'}, ['10m', '100m', '650m', '700m', '1g', '4480m'].map(v => h('option', {value:v})));
  const params = h('input', {class:'in', placeholder:'-mqs=on', autocomplete:'off', spellcheck:'false'});
  const upd = sel(); [['add', 'uAdd'], ['update', 'uUpdate'], ['fresh', 'uFresh'], ['sync', 'uSync']].forEach(([v, k]) => upd.append(opt(v, t(k))));
  const pmode = sel(); [['rel', 'pmRel'], ['full', 'pmFull'], ['abs', 'pmAbs']].forEach(([v, k]) => pmode.append(opt(v, t(k))));
  const cSfx = h('input', {type:'checkbox'}), cShared = h('input', {type:'checkbox'}), cDel = h('input', {type:'checkbox'});
  const pw1 = h('input', {type:'password', class:'in', autocomplete:'new-password'}), pw2 = h('input', {type:'password', class:'in', autocomplete:'new-password'});
  const cShow = h('input', {type:'checkbox'});
  const encMethod = sel(); encMethod.append(opt('AES256', 'AES-256'), opt('ZipCrypto', 'ZipCrypto'));
  const cEncNames = h('input', {type:'checkbox'});
  const warn = h('div', {class:'cwarn'});
  const checks = [];
  const chk = (key, el) => { const lb = h('label', null, el, ' ' + t(key)); checks.push([lb, el]); return lb; };

  function fillSel(el, items, prev){   // items: [[value,label]]
    el.replaceChildren(...items.map(([v, l]) => opt(v, l)));
    el.value = items.some(([v]) => String(v) === String(prev)) ? prev : (items.length ? items[0][0] : '');
  }
  const ext = () => (fmt.value === '7z' && cSfx.checked) ? (isWin() ? '.exe' : '.sfx') : FEXT[fmt.value];
  function stripExt(name){
    const low = name.toLowerCase();
    for (const e of ALLEXT) if (low.endsWith(e)) return name.slice(0, name.length - e.length);
    return name;
  }
  function suggest(){
    if (!sources.length){ if (outAuto) out.value = ''; return; }
    const s0 = sources[0], f = s0.p.replace(/[\\/]+$/, '');
    let base = sources.length === 1 ? basename(f) : (basename(dirname(f)) || 'archive');
    if (sources.length === 1 && !s0.d && !SINGLE.includes(fmt.value)) base = base.replace(/\.[^.]*$/, '') || base;
    if (outAuto) out.value = join(dirname(f), base + ext());
  }
  function swapExt(){ if (out.value.trim()) out.value = stripExt(out.value.trim()) + ext(); }
  function refresh(){
    list.replaceChildren(...(sources.length ? sources.map((s, i) => h('div', {class:'src'},
      h('span', null, s.d ? '📁' : '📄'), h('span', {class:'sp', title:s.p}, s.p),
      h('button', {class:'x', title:t('cRemove'), onclick: () => { sources.splice(i, 1); refresh(); }}, '✕'))) : [h('div', {class:'mut'}, t('cNone'))]));
    suggest(); sync();
  }
  function sync(){
    const f = fmt.value, lv = +level.value;
    const isTar = f === 'tar', isWim = f === 'wim', isTx = f.startsWith('tar.');
    const store = lv === 0 && (f === '7z' || f === 'zip');
    // method
    let ms = FMETH[f] || [];
    if (store) ms = ['Copy'];
    fillSel(method, ms.map(m => [m, m]), method.value);
    method.disabled = ms.length <= 1;
    const eff = method.value;
    level.disabled = isTar || isWim;
    // dictionary / word size
    const dOpts = (isTar || isWim || store) ? null : dictOpts(eff);
    fillSel(dict, [['', t('cAuto')]].concat((dOpts || []).map(v => [v, dictLabel(v)])), dict.value); dict.disabled = !dOpts;
    const wOpts = (isTar || isWim || store) ? null : wordOpts(eff);
    fillSel(word, [['', t('cAuto')]].concat((wOpts || []).map(v => [v, v])), word.value); word.disabled = !wOpts;
    fillSel(solid, [['', t('cAuto')]].concat(SOLIDS.map(v => [v, v === 'off' ? t('solidOff') : v === 'on' ? t('solidOn') : dictLabel(v)])), solid.value);
    solid.disabled = f !== '7z' || store;
    threads.disabled = memuse.disabled = !THREADED.includes(f) || store;
    vol.disabled = !['7z', 'zip', 'tar'].includes(f);
    const canUpd = ['7z', 'zip', 'tar', 'wim'].includes(f);
    upd.disabled = !canUpd; if (!canUpd) upd.value = 'add';
    cSfx.disabled = f !== '7z'; if (f !== '7z') cSfx.checked = false;
    cShared.disabled = !(S.info && S.info.win);
    cDel.disabled = isTx; if (isTx) cDel.checked = false;
    const enc = f === '7z' || f === 'zip';
    pw1.disabled = pw2.disabled = cShow.disabled = !enc;
    if (!enc){ pw1.value = pw2.value = ''; }
    encMethod.disabled = f !== 'zip'; if (f !== 'zip') encMethod.value = 'AES256';
    cEncNames.disabled = f !== '7z' || !pw1.value; if (cEncNames.disabled) cEncNames.checked = false;
    for (const [lb, el] of fields) lb.classList.toggle('dis', !!el.disabled);
    for (const [lb, el] of checks) lb.classList.toggle('dis', !!el.disabled);
    const bad = SINGLE.includes(f) && (sources.length > 1 || sources.some(s => s.d));
    warn.textContent = bad ? t('cSingleWarn', f) : ''; warn.style.display = bad ? 'block' : 'none';
  }
  [level, method].forEach(el => el.addEventListener('change', sync));
  pw1.addEventListener('input', sync);
  fmt.addEventListener('change', () => { suggest(); swapExt(); sync(); });
  cSfx.addEventListener('change', () => { swapExt(); sync(); });
  cShow.addEventListener('change', () => { pw1.type = pw2.type = cShow.checked ? 'text' : 'password'; });

  const leftCol = h('div', null,
    fld('cFormat', fmt), fld('cLevel', level), fld('cMethod', method), fld('cDict', dict), fld('cWord', word),
    fld('cSolid', solid), fld('cThreads', threads), fld('cMem', memuse));
  const rightCol = h('div', null,
    fld('cUpdate', upd), fld('cPath', pmode),
    h('fieldset', {class:'grp'}, h('legend', null, t('cOpts')), chk('cSfx', cSfx), isWin() ? chk('cShared', cShared) : null, chk('cDel', cDel)),
    fld('cVol', vol), fld('cParams', params));
  const encBox = h('fieldset', {class:'grp'}, h('legend', null, t('cEnc')),
    h('div', {class:'cgrid'},
      h('div', null, fld('cPw', pw1), fld('cPw2', pw2), h('label', null, cShow, ' ' + t('cShowPw'))),
      h('div', null, fld('cEncMethod', encMethod), chk('cEncNames', cEncNames))));
  const body = h('div', null,
    h('div', {style:'margin-bottom:6px'}, t('cItems')),
    h('div', {class:'row top'}, list, h('button', {class:'btn', onclick: async () => {
      const r = await pickPath({mode:'multi', title:t('cPickItems')});
      if (r) for (const x of r) if (!sources.some(s => s.p === x.p)) sources.push(x);
      refresh();
    }}, t('cAddMore'))),
    h('div', {class:'row'}, h('label', null, t('cArchive')), out,
      h('button', {class:'btn', onclick: async () => {
        const d = await pickPath({mode:'dir', title:t('cPickOut'), start: out.value ? dirname(out.value) : ''});
        if (d){ out.value = join(d, basename(out.value) || ('archive' + ext())); outAuto = false; }
      }}, t('cFolderBtn'))),
    warn,
    h('div', {class:'cgrid'}, leftCol, rightCol), encBox, vlist);

  const m = openModal(t('cTitle'), body, [{label:t('cancel'), value:null}, {label:t('create'), kind:'primary', action: async () => {
    if (!sources.length){ await alertBox(t('cNoItems')); return false; }
    if (!out.value.trim()){ out.focus(); return false; }
    if (warn.style.display === 'block'){ await alertBox(warn.textContent); return false; }
    if (pw1.value !== pw2.value){ await alertBox(t('cPwMismatch')); return false; }
    if (cDel.checked){
      const c = await choose(t('cDelConfirm'), [{label:t('cancel'), value:null}, {label:t('ok'), kind:'danger', value:'y'}]);
      if (c !== 'y') return false;
    }
    const lv = +level.value;
    return {sources:sources.map(s => s.p), out:out.value.trim(), fmt:fmt.value, level:lv,
      method: method.disabled && !FMETH[fmt.value] ? '' : method.value,
      dict: dict.disabled ? '' : dict.value, word: word.disabled ? '' : word.value,
      solid: solid.disabled ? '' : solid.value, threads: threads.disabled ? '' : threads.value,
      memuse: memuse.disabled ? '' : memuse.value, volume: vol.disabled ? '' : vol.value.trim(),
      params: params.value.trim(), update: upd.value, pathmode: pmode.value,
      sfx: cSfx.checked, shared: cShared.checked, sdel: cDel.checked,
      password: pw1.value, enc_method: encMethod.value, enc_names: cEncNames.checked};
  }}], {width:800});
  refresh();
  const r = await m.promise; if (!r) return;
  let res;
  try { res = await api('/api/create', r); } catch(e){ await alertBox(e.message); return; }
  if (res.exists){
    const btns = [{label:t('cancel'), value:null}, {label:t('cOverwrite'), kind:'danger', value:'overwrite'}];
    if (!res.noadd) btns.push({label:t('cAddTo'), kind:'primary', value:'add'});
    const c = await choose(t('cExists', r.out), btns);
    if (!c) return;
    r.if_exists = c;
    try { res = await api('/api/create', r); } catch(e){ await alertBox(e.message); return; }
  }
  if (res.error){ await alertBox(res.error); return; }
  const pr = await runProgress(t('tCreate'), res.job, {verb:'vAdd'});
  if (pr && (pr.rc === 0 || pr.rc === 1)){
    status('cCreated', res.out);
    await openArchive(res.open || res.out, r.password);
  }
}

/* ---------- size of the selected items ---------- */
function selSizing(){
  const sel = selectedNodes().filter(n => {
    for (let p = n.parent; p && p.key !== ''; p = p.parent) if (S.selected.has(p.key)) return false;
    return true;
  });
  const total = sel.reduce((a, n) => a + (n.dir ? n.total : (n.size || 0)), 0);
  const files = sel.reduce((a, n) => a + (n.dir ? n.nfiles : 1), 0);
  const all = S.stats ? S.stats.size : 0;
  const packed = all ? Math.round(S.stats.packed * total / all) : 0;
  return {total, packed, files, sel};
}

/* ---------- archive properties ---------- */
async function showProps(){
  if (!S.archive){ await alertBox(t('openFirst')); return; }
  const st = S.stats;
  const secs = [
    [[t('pSize'), grp(st.size)], [t('pPacked'), grp(st.packed)], [t('pFolders'), grp(st.folders)], [t('pFilesN'), grp(st.files)]],
    (S.props || []).map(([k, v]) => [tProp(k), /Size|Offset|^Blocks$/.test(k) && /^\d+$/.test(v) ? grp(v) : v]),
  ];
  const tbl = h('table', {class:'ptab'});
  secs.forEach((sec, i) => {
    if (i) tbl.append(h('tr', {class:'sep'}, h('td', {colspan:2})));
    for (const [k, v] of sec) tbl.append(h('tr', null, h('td', {class:'k'}, k), h('td', {class:'val'}, v)));
  });
  await openModal(t('propTitle'), h('div', {class:'pwrap'}, tbl),
    [{label:t('cancel'), value:null}, {label:t('ok'), kind:'primary', value:true}], {width:660}).promise;
}

/* ---------- checksum ---------- */
async function showHash(res, label){
  const kv = [[t('hName'), label]];
  if (res.files > 1) kv.push([t('pFilesN'), grp(res.files)]);
  if (res.size != null) kv.push([t('pSize'), t('hBytes', res.size, Math.floor(res.size / 1024))]);
  for (const r of res.rows){
    if (r.data) kv.push([r.names ? t('hData', r.h) : r.h, r.data]);
    if (r.names) kv.push([t('hDataNames', r.h), r.names]);
  }
  const text = kv.map(([k, v]) => k + '\t' + v).join('\n');
  const body = h('div', null, h('div', {class:'hgrid'}, kv.flatMap(([k, v]) => [h('div', {class:'hk'}, k), h('div', {class:'hv'}, v)])));
  if (res.items.length > 1){
    const cols = Object.keys(res.items[0].hashes);
    const tb = h('table', {class:'ptab hitems'},
      h('tr', null, h('th', null, t('hName')), h('th', null, t('pSize')), cols.map(c => h('th', null, c))),
      res.items.map(it => h('tr', null, h('td', {class:'val'}, it.name), h('td', null, it.size == null ? '' : grp(it.size)),
        cols.map(c => h('td', {class:'hv'}, it.hashes[c] || '')))));
    body.append(h('details', null, h('summary', null, t('hPerFile', res.items.length)),
      h('div', {style:'overflow:auto;max-height:240px;margin-top:6px'}, tb)));
  }
  const m = openModal(t('hTitle'), h('div', {class:'pwrap'}, body), [
    {label:t('copy'), action: async () => {
      const ok = await copyText(text); m.buttons[0].el.textContent = ok ? t('copied') : t('copyFail'); return false; }},
    {label:t('ok'), kind:'primary', value:true}], {width:780});
  await m.promise;
}
async function hashFlow(){
  const hasArc = !!S.archive;
  const sel = hasArc ? selSizing() : null;
  const btns = [{label:t('cancel'), value:null}];
  if (hasArc) btns.push({label: sel.sel.length ? t('hSrcSel', sel.sel.length) : t('hSrcAll'), value:'arc'});
  btns.push({label:t('hSrcDisk'), kind:'primary', value:'disk'});
  const c = await choose(t('hChoose'), btns, t('bHash'));
  if (!c) return;
  let res, label;
  try {
    if (c === 'disk'){
      const picked = await pickPath({mode:'multi', title:t('hPick')});
      if (!picked || !picked.length) return;
      const paths = picked.map(x => x.p);
      label = paths.length === 1 ? basename(paths[0]) : t('selCount', paths.length);
      res = await api('/api/hash', {paths});
    } else {
      const z = sel.sel.length ? sel : {total:S.stats.size, packed:S.stats.packed, files:S.stats.files};
      label = basename(S.archive) + (sel.sel.length ? ' → ' + sel.sel.length : '');
      res = await api('/api/hash', {archive:S.archive, password:S.password, items:sel.sel.map(n => ({p:n.orig, d:n.dir})),
        total:z.total, packed:z.packed, files:z.files});
    }
  } catch(e){ await alertBox(e.message); return; }
  if (res.error){ await alertBox(res.error); return; }
  const pr = await runProgress(t('tHash'), res.job, {verb:'vHash'});
  if (!pr || pr.cancelled || (pr.rc !== 0 && pr.rc !== 1)) return;
  let hr;
  try { hr = await api('/api/hashresult?id=' + res.job); } catch(e){ await alertBox(e.message); return; }
  if (hr.error){ await alertBox(hr.error); return; }
  if (!hr.rows.length){ await alertBox(t('hNoResult')); return; }
  await showHash(hr, label);
}

/* ---------- benchmark ---------- */
async function benchFlow(){
  const info = S.info || {}, hw = info.threads || 1;
  const sel = (vals, fmt) => h('select', {class:'in', style:'width:auto'}, vals.map(n => h('option', {value:n}, fmt ? fmt(n) : n)));
  const dictSel = sel([20, 21, 22, 23, 24, 25, 26, 27, 28], n => (1 << (n - 20)) + ' MB'); dictSel.value = '24';
  const thrSel = sel(Array.from({length:hw}, (_, i) => i + 1)); thrSel.value = String(hw);
  const passSel = sel([1, 2, 3, 4, 5, 10]);
  const memEl = h('span', null, '—'), elEl = h('span', null, '00:00:00');
  const btnRestart = h('button', {class:'btn'}, t('bnStart')), btnStop = h('button', {class:'btn'}, t('bnStop'));
  btnStop.disabled = true;
  const grid = title => {
    const mk = () => ({dict:h('td'), speed:h('td'), usage:h('td'), ru:h('td'), rating:h('td')});
    const cur = mk(), res = mk();
    const row = (name, c) => h('tr', null, h('td', {class:'k'}, name), c.dict, c.speed, c.usage, c.ru, c.rating);
    return {cur, res, el: h('fieldset', {class:'grp'}, h('legend', null, title), h('table', {class:'btab'},
      h('tr', null, ['', t('bnCDict'), t('bnCSpeed'), t('bnCUsage'), t('bnCRU'), t('bnCRating')].map(x => h('th', null, x))),
      row(t('bnCur'), cur), row(t('bnRes'), res)))};
  };
  const comp = grid(t('bnComp')), dec = grid(t('bnDec'));
  const tCpu = h('span'), tRu = h('span'), tRt = h('span');
  const totalBox = h('fieldset', {class:'grp', style:'margin:0'}, h('legend', null, t('bnTotal')),
    h('div', {class:'ttab'}, h('span', {class:'th'}, t('bnCUsage')), h('span', {class:'th'}, t('bnCRU')), h('span', {class:'th'}, t('bnCRating')), tCpu, tRu, tRt));
  const freqEl = h('pre', {class:'freq'}), rawEl = h('pre', {class:'rawlog'}), errEl = h('div', {class:'plbl'});
  const banner = (info.banner || '').replace(/\s*:\s*Copyright.*$/, '');
  const body = h('div', null,
    h('div', {class:'bgrid'},
      h('div', null,
        h('div', {class:'row'}, h('label', null, t('bnDict')), dictSel, h('span', {style:'flex:1'}), btnRestart),
        h('div', {class:'row'}, h('label', null, t('bnThreads')), thrSel, h('span', {class:'dim'}, '/ ' + hw), h('span', {style:'flex:1'}), btnStop),
        h('div', {class:'row'}, h('label', null, t('bnMem')), memEl),
        comp.el, dec.el,
        h('div', {class:'row', style:'align-items:flex-start;margin-bottom:0'},
          h('div', {style:'flex:1'},
            h('div', {class:'row'}, h('label', null, t('bnElapsed')), elEl),
            h('div', {class:'row'}, h('label', null, t('bnPasses')), passSel)),
          totalBox)),
      freqEl),
    errEl,
    h('div', {class:'binfo'}, info.cpu || '', h('br'), info.os || '', h('br'), banner + (info.arch ? '  ·  ' + info.arch + ' threads:' + hw : '')),
    h('details', null, h('summary', null, t('bnRaw')), rawEl));
  const m = openModal(t('bnTitle'), body, [{label:t('close'), value:true}], {width:920});

  let rows = [], avr = null, tot = null, freq = [], rawLines = [], ramUse = null, ramTotal = null;
  let jobId = null, since = 0, running = false, closed = false;
  const fmtDict = n => { const b = Math.pow(2, n); return b >= 1048576 ? (b / 1048576) + ' MB' : (b / 1024) + ' KB'; };
  const G = v => (v / 1000).toFixed(3) + ' GIPS';
  function setRow(c, v){
    const put = (el, x) => { el.textContent = v ? x : ''; };
    put(c.dict, v && fmtDict(v.dict)); put(c.speed, v && Math.round(v.speed) + ' KB/s'); put(c.usage, v && Math.round(v.usage) + '%');
    put(c.ru, v && G(v.ru)); put(c.rating, v && G(v.rating));
  }
  const meanOf = list => {
    const avg = (k, i) => list.reduce((a, r) => a + r[k][i], 0) / list.length;
    return {c:[0, 1, 2, 3].map(i => avg('c', i)), d:[0, 1, 2, 3].map(i => avg('d', i))};
  };
  function draw(){
    const last = rows[rows.length - 1];
    const res = avr || (rows.length ? meanOf(rows) : null);
    const rv = (x, k) => x && last ? {dict:last.dict, speed:x[k][0], usage:x[k][1], ru:x[k][2], rating:x[k][3]} : null;
    setRow(comp.cur, rv(last, 'c')); setRow(comp.res, rv(res, 'c'));
    setRow(dec.cur, rv(last, 'd')); setRow(dec.res, rv(res, 'd'));
    const tt = tot || (res ? [(res.c[1] + res.d[1]) / 2, (res.c[2] + res.d[2]) / 2, (res.c[3] + res.d[3]) / 2] : null);
    tCpu.textContent = tt ? Math.round(tt[0]) + '%' : '—'; tRu.textContent = tt ? G(tt[1]) : '—'; tRt.textContent = tt ? G(tt[2]) : '—';
    memEl.textContent = ramUse != null ? ramUse + ' MB / ' + (ramTotal != null ? ramTotal : (info.ram || '?')) + ' MB' : '—';
    freqEl.textContent = freq.join('\n'); rawEl.textContent = rawLines.join('\n');
  }
  function parse(lines){
    for (const ln of lines){
      rawLines.push(ln); let x;
      if ((x = ln.match(/RAM size:\s+(\d+) MB/))) ramTotal = +x[1];
      else if ((x = ln.match(/RAM usage:\s+(\d+) MB/))) ramUse = +x[1];
      else if (/CPU Freq/i.test(ln)) freq.push(ln.trim());
      else if ((x = ln.match(/^(\d+):\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s+\|\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s*$/)))
        rows.push({dict:+x[1], c:[+x[2], +x[3], +x[4], +x[5]], d:[+x[6], +x[7], +x[8], +x[9]]});
      else if ((x = ln.match(/^Avr:\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s+\|\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)/)))
        avr = {c:[+x[1], +x[2], +x[3], +x[4]], d:[+x[5], +x[6], +x[7], +x[8]]};
      else if ((x = ln.match(/^Tot:\s+(\d+)\s+(\d+)\s+(\d+)/))) tot = [+x[1], +x[2], +x[3]];
    }
  }
  async function stop(){
    if (jobId && running){ running = false; try { await api('/api/cancel', {job:jobId}); } catch(e){} }
    btnStop.disabled = true;
  }
  async function tick(id){
    if (closed || id !== jobId) return;
    let r; try { r = await api('/api/job?id=' + id + '&since=' + since); } catch(e){ return; }
    if (closed || id !== jobId || r.error) return;
    since = r.next; parse(r.lines); elEl.textContent = fmtHMS(r.el); draw();
    if (r.done){
      running = false; btnStop.disabled = true; btnRestart.textContent = t('bnRestart');
      if (r.rc !== 0 && !r.cancelled){ errEl.textContent = t('bnErr', r.rc); errEl.classList.add('err'); }
      return;
    }
    setTimeout(() => tick(id), 300);
  }
  async function start(){
    await stop();
    rows = []; avr = null; tot = null; freq = []; rawLines = []; ramUse = null; since = 0; elEl.textContent = '00:00:00';
    errEl.textContent = ''; errEl.classList.remove('err'); draw();
    let res;
    try { res = await api('/api/bench', {threads:+thrSel.value, dict:+dictSel.value, passes:+passSel.value}); }
    catch(e){ errEl.textContent = e.message; errEl.classList.add('err'); return; }
    if (res.error){ errEl.textContent = res.error; errEl.classList.add('err'); return; }
    jobId = res.job; running = true; btnStop.disabled = false; btnRestart.textContent = t('bnRestart');
    tick(res.job);
  }
  btnRestart.addEventListener('click', start);
  btnStop.addEventListener('click', stop);
  m.promise.then(() => { closed = true; stop(); });
  draw(); start();
}

/* ---------- toolbar, preferences, start ---------- */
function fillPrefs(){
  const th = $('#selTheme'), lg = $('#selLang');
  th.replaceChildren(...[['auto', 'themeAuto'], ['light', 'themeLight'], ['dark', 'themeDark'], ['oled', 'themeOled']].map(([v, k]) => opt(v, t(k))));
  th.value = THEME;
  lg.replaceChildren(...LANGS.map(([c, n]) => opt(c, n)));
  lg.value = LANG;
}
function updInfoBar(){
  if (!S.info) return;
  const w = $('#warn');
  if (!S.info.exe){ w.style.display = 'block'; w.textContent = t('no7z', S.info.here); }
  else { w.style.display = 'none'; $('#ver').textContent = S.info.banner.replace(/\s*:\s*Copyright.*$/, '') + '  —  ' + S.info.exe; }
}
function applyI18n(){
  document.documentElement.lang = LANG;
  document.querySelectorAll('[data-i18n]').forEach(el => { el.textContent = t(el.dataset.i18n); });
  document.querySelectorAll('[data-i18n-title]').forEach(el => { el.title = t(el.dataset.i18nTitle); });
  fillPrefs(); updInfoBar();
  if (S.st) status(S.st[0], ...S.st[1]);
  render();
}
$('#selTheme').addEventListener('change', e => { applyTheme(e.target.value); lsSet('theme', THEME); });
$('#selLang').addEventListener('change', e => { LANG = e.target.value; lsSet('lang', LANG); applyI18n(); });
$('#bNew').addEventListener('click', () => createDialog([]));
$('#bOpen').addEventListener('click', chooseArchive);
$('#bExtract').addEventListener('click', extractDialog);
$('#bTest').addEventListener('click', testArchive);
$('#bInfo').addEventListener('click', showProps);
$('#bHash').addEventListener('click', hashFlow);
$('#bBench').addEventListener('click', benchFlow);
$('#bQuit').addEventListener('click', async () => {
  const c = await choose(t('quitAsk'), [{label:t('cancel'), value:null}, {label:t('bQuit'), kind:'danger', value:'q'}]);
  if (c !== 'q') return;
  try { await api('/api/quit', {}); } catch(e){}
  document.body.innerHTML = '<div style="padding:40px;text-align:center;font-size:16px">' + esc(t('quitDone')) + '</div>';
  window.close();
});
setInterval(() => { api('/api/ping').catch(() => {}); }, 5000);
(async function init(){
  LANG = detectLang();
  applyTheme(lsGet('theme') || 'auto');
  status('ready'); applyI18n();
  try {
    S.info = await api('/api/info');
    if (!S.info.exe) status('no7zShort');
    else status('readyWith', S.info.banner.replace(/\s*:\s*Copyright.*$/, ''));
    updInfoBar();
  } catch(e){ status('noServer'); return; }
  render();
  if (INITIAL && INITIAL.open) openArchive(INITIAL.open);
  else if (INITIAL && INITIAL.add && INITIAL.add.length) createDialog(INITIAL.add);
})();
})();
</script></body></html>
"""


def render_page(initial=None):
    data = json.dumps(STATE.get("initial") or {"open": None, "add": []}, ensure_ascii=False)
    data = data.replace("</", "<\\/")
    return PAGE.replace("/*__INITIAL__*/null", data)


# --------------------------------------------------------------------------
# Baslatma
# --------------------------------------------------------------------------
def browser_candidates():
    names = ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser",
             "brave-browser", "microsoft-edge", "msedge", "chrome"]
    paths = []
    if IS_WIN:
        for env in ("ProgramFiles", "ProgramFiles(x86)", "LocalAppData"):
            root = os.environ.get(env)
            if root:
                paths += [os.path.join(root, r"Microsoft\Edge\Application\msedge.exe"),
                          os.path.join(root, r"Google\Chrome\Application\chrome.exe"),
                          os.path.join(root, r"BraveSoftware\Brave-Browser\Application\brave.exe")]
    elif IS_MAC:
        paths = ["/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
                 "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
                 "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
                 "/Applications/Chromium.app/Contents/MacOS/Chromium"]
    return [shutil.which(n) for n in names] + paths


def launch_browser(url, use_app):
    if use_app:
        for b in browser_candidates():
            if b and os.path.isfile(b):
                try:
                    kw = {} if IS_WIN else {"start_new_session": True}
                    subprocess.Popen([b, "--app=" + url, "--window-size=1100,720"],
                                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, **kw)
                    return
                except OSError:
                    pass
    try:
        webbrowser.open(url)
    except Exception:  # noqa: BLE001
        pass


def watchdog():
    while True:
        time.sleep(5)
        busy = any(not j.done.is_set() for j in list(JOBS.values()))
        if not busy and time.time() - STATE["last_ping"] > IDLE_LIMIT:
            print("Browser connection lost, shutting down.")
            STATE["server"].shutdown()
            return


def main():
    ap = argparse.ArgumentParser(description="7Zip GUI (no dependencies)")
    ap.add_argument("paths", nargs="*", help="archive to open (or items to add with --add)")
    ap.add_argument("--add", action="store_true", help="open the Add to Archive dialog with these paths")
    ap.add_argument("--no-browser", action="store_true", help="do not open a browser, just print the URL")
    ap.add_argument("--no-app", action="store_true", help="do not use the Chromium app-window mode")
    ap.add_argument("--port", type=int, default=0)
    a = ap.parse_args()

    exe, banner = find_7z()
    STATE["exe"], STATE["banner"] = exe, banner
    m = re.search(r"(\d+\.\d+)", banner)
    STATE["version"] = m.group(1) if m else ""
    if exe:
        print(f"7-Zip: {exe}  ({banner.split(':')[0].strip()})")
    else:
        print("WARNING: 7-Zip not found. Put 7zz / 7z.exe into this folder:", HERE)

    initial = {"open": None, "add": []}
    if a.add:
        initial["add"] = [os.path.abspath(p) for p in a.paths]
    elif a.paths:
        initial["open"] = os.path.abspath(a.paths[0])
    STATE["initial"] = initial

    STATE["token"] = secrets.token_urlsafe(24)
    STATE["tmp"] = tempfile.mkdtemp(prefix="7zgui-")
    srv = Server(("127.0.0.1", a.port), Handler)
    STATE["server"] = srv
    STATE["port"] = srv.server_address[1]
    url = f"http://127.0.0.1:{STATE['port']}/?t={STATE['token']}"
    print("URL:", url)
    print("Use the Quit button in the browser or Ctrl+C to stop.")
    STATE["last_ping"] = time.time()
    threading.Thread(target=watchdog, daemon=True).start()
    if not a.no_browser:
        launch_browser(url, not a.no_app)
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        for j in list(JOBS.values()):
            j.cancel()
        shutil.rmtree(STATE["tmp"], ignore_errors=True)
        srv.server_close()


if __name__ == "__main__":
    main()
