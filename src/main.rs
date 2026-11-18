#![allow(dead_code, deprecated)]
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
mod app;
mod core;
mod dialogs;
mod i18n;
mod picker;
mod settings;
mod shell;
use app::{Startup, Task};
use eframe::egui;
use std::path::PathBuf;

const HELP: &str = "Universal 7-Zip GUI (u7zgui)

  u7zgui                         open the main window
  u7zgui <archive>               open an archive
  u7zgui <files or folders>      open \"Add to archive\" for them
  u7zgui open <archive>
  u7zgui add <paths...>          \"Add to archive...\" dialog
  u7zgui quick-add <fmt> <paths...>   archive with defaults, progress only (fmt: 7z zip tar gzip bzip2 xz wim tar.gz tar.bz2 tar.xz)
  u7zgui extract <archive>       \"Extract...\" dialog
  u7zgui extract-here <archives...>
  u7zgui extract-to <archives...>     extract into a folder named after the archive
  u7zgui test <archives...>
  u7zgui hash <paths...>         checksum window
  u7zgui install-shell           add the right-click menu entries of your file manager
  u7zgui uninstall-shell         remove them again

7-Zip (7zz / 7z / 7z.exe) is searched next to this program first, then in PATH.
";

fn paths(a: &[String]) -> Vec<PathBuf> {
    a.iter().map(|s| std::fs::canonicalize(s).map(|p| strip_unc(p)).unwrap_or_else(|_| PathBuf::from(s))).collect()
}
fn strip_unc(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy().to_string();
    PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s).to_string())
}

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut start = Startup::default();
    start.shot = std::env::var("SHOT").ok().map(|p| (PathBuf::from(p), std::env::var("SHOT_FRAME").ok().and_then(|v| v.parse().ok()).unwrap_or(40)));
    start.script = std::env::var("U7Z_SCRIPT").unwrap_or_default();
    let mut size = if std::env::var("U7Z_SIZE").is_ok() { [1000.0, 720.0] } else { [980.0, 640.0] };
    if let Some(cmd) = args.first() {
        let rest = &args[1..];
        match cmd.as_str() {
            "-h" | "--help" | "help" => {
                print!("{HELP}");
                return Ok(());
            }
            "install-shell" | "uninstall-shell" => {
                let r = if cmd == "install-shell" { shell::install() } else { shell::uninstall() };
                match r {
                    Ok(()) => println!("OK"),
                    Err(e) => {
                        eprintln!("{e}");
                        std::process::exit(1);
                    }
                }
                return Ok(());
            }
            "open" => start.open = rest.first().map(|s| paths(&[s.clone()]).remove(0)),
            "add" => start.add = paths(rest),
            "extract" => start.extract_dialog = rest.first().map(|s| paths(&[s.clone()]).remove(0)),
            "hash" => start.hash = paths(rest),
            "extract-here" => start.batch = paths(rest).into_iter().map(Task::ExtractHere).collect(),
            "extract-to" => start.batch = paths(rest).into_iter().map(Task::ExtractTo).collect(),
            "test" => start.batch = paths(rest).into_iter().map(Task::Test).collect(),
            "quick-add" => {
                if rest.len() >= 2 {
                    start.batch = vec![Task::QuickAdd { fmt: rest[0].clone(), sources: paths(&rest[1..]) }];
                }
            }
            _ => {
                let p = paths(&args);
                if p.len() == 1 && p[0].is_file() && app::looks_like_archive(&p[0]) {
                    start.open = Some(p[0].clone());
                } else {
                    start.add = p;
                }
            }
        }
    }
    if !start.batch.is_empty() {
        size = [520.0, 330.0];
    }
    let mut vp = egui::ViewportBuilder::default().with_inner_size(size).with_title("Universal 7-Zip GUI");
    if start.batch.is_empty() {
        vp = vp.with_min_inner_size([640.0, 400.0]);
    }
    let opts = eframe::NativeOptions { viewport: vp, centered: true, ..Default::default() };
    eframe::run_native("Universal 7-Zip GUI", opts, Box::new(move |cc| Ok(Box::new(app::App::new(cc, start)))))
}
