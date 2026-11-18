//! Right-click integration for the file managers of each OS ("install-shell" / "uninstall-shell").
//! Everything is written into the user's own profile; nothing needs administrator rights.
use crate::i18n::{self, I18n};
use crate::settings::Settings;
use std::path::PathBuf;

const ARCHIVE_EXTS: &[&str] = &[
    "7z", "zip", "tar", "gz", "tgz", "bz2", "tbz2", "xz", "txz", "wim", "rar", "iso", "cab", "vhd", "vhdx", "dmg", "img", "xar", "lzh", "lha", "arj", "cpio", "rpm", "deb", "lzma", "zst", "jar", "apk", "001",
];
#[allow(dead_code)]
const ARCHIVE_MIMES: &[&str] = &[
    "application/zip", "application/x-7z-compressed", "application/x-tar", "application/gzip", "application/x-gzip", "application/x-bzip2", "application/x-bzip", "application/x-xz",
    "application/x-compressed-tar", "application/x-bzip-compressed-tar", "application/x-bzip2-compressed-tar", "application/x-xz-compressed-tar", "application/vnd.rar", "application/x-rar",
    "application/x-cd-image", "application/x-iso9660-image", "application/vnd.ms-cab-compressed", "application/x-cpio", "application/x-rpm", "application/vnd.debian.binary-package",
    "application/x-archive", "application/x-lzma", "application/zstd", "application/x-zstd-compressed-tar", "application/x-apple-diskimage", "application/java-archive", "application/x-ms-wim",
];

fn exe_path() -> Result<PathBuf, String> {
    let p = std::env::current_exe().map_err(|e| e.to_string())?;
    Ok(std::fs::canonicalize(&p).unwrap_or(p))
}

fn tr() -> I18n {
    let s = Settings::load();
    I18n::load(&if s.lang.is_empty() { i18n::system_lang() } else { s.lang })
}

/// One menu entry: label, command-line arguments (before the file list), only for archives?
struct Item {
    id: &'static str,
    label: String,
    args: &'static str,
    archives_only: bool,
}

fn items(t: &I18n) -> Vec<Item> {
    vec![
        Item { id: "extract", label: t.t("mExtractFiles"), args: "extract", archives_only: true },
        Item { id: "here", label: t.t("mExtractHere"), args: "extract-here", archives_only: true },
        Item { id: "to", label: t.tf("mExtractTo", &["…"]), args: "extract-to", archives_only: true },
        Item { id: "test", label: t.t("mTestArchive"), args: "test", archives_only: true },
        Item { id: "add", label: t.t("mAddArchive"), args: "add", archives_only: false },
        Item { id: "q7z", label: t.tf("mAddTo", &["….7z"]), args: "quick-add 7z", archives_only: false },
        Item { id: "qzip", label: t.tf("mAddTo", &["….zip"]), args: "quick-add zip", archives_only: false },
        Item { id: "hash", label: t.t("mChecksum"), args: "hash", archives_only: false },
    ]
}

#[allow(dead_code)]
fn xml_esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

pub fn install() -> Result<(), String> {
    let exe = exe_path()?;
    let t = tr();
    #[cfg(target_os = "linux")]
    return linux::install(&exe, &t);
    #[cfg(windows)]
    return windows::install(&exe, &t);
    #[cfg(target_os = "macos")]
    return macos::install(&exe, &t);
    #[allow(unreachable_code)]
    {
        let _ = (exe, t);
        Err("unsupported platform".into())
    }
}

pub fn uninstall() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    return linux::uninstall();
    #[cfg(windows)]
    return windows::uninstall();
    #[cfg(target_os = "macos")]
    return macos::uninstall();
    #[allow(unreachable_code)]
    Err("unsupported platform".into())
}

// ---------------------------------------------------------------------------------------------
// Linux: Dolphin, Nemo, Nautilus / Caja scripts, Thunar, plus an "Open with" launcher
// ---------------------------------------------------------------------------------------------
#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    fn home() -> PathBuf {
        std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
    }
    fn data_home() -> PathBuf {
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| home().join(".local/share"))
    }
    fn config_home() -> PathBuf {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| home().join(".config"))
    }
    fn write(p: &Path, text: &str, exec: bool) -> Result<(), String> {
        if let Some(d) = p.parent() {
            fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
        }
        fs::write(p, text).map_err(|e| format!("{}: {e}", p.display()))?;
        if exec {
            let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o755));
        }
        Ok(())
    }
    /// Desktop-entry Exec quoting: the program path in double quotes.
    fn q(exe: &Path) -> String {
        format!("\"{}\"", exe.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"").replace('$', "\\$").replace('`', "\\`"))
    }
    fn sh_q(s: &str) -> String {
        format!("'{}'", s.replace('\'', "'\\''"))
    }

    const ICON_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><rect x="4" y="4" width="56" height="56" rx="12" fill="#1f2937"/><rect x="28" y="4" width="8" height="30" fill="#e5e7eb"/><g fill="#1f2937"><rect x="28" y="8" width="4" height="4"/><rect x="32" y="14" width="4" height="4"/><rect x="28" y="20" width="4" height="4"/><rect x="32" y="26" width="4" height="4"/></g><text x="32" y="55" font-family="sans-serif" font-size="16" font-weight="700" text-anchor="middle" fill="#60a5fa">7z</text></svg>"##;

    pub fn install(exe: &Path, t: &I18n) -> Result<(), String> {
        let its = items(t);
        let mut done = 0;
        let mut errs: Vec<String> = vec![];
        let mut run = |r: Result<(), String>| match r {
            Ok(()) => done += 1,
            Err(e) => errs.push(e),
        };
        let dh = data_home();
        run(write(&dh.join("icons/hicolor/scalable/apps/u7z.svg"), ICON_SVG, false));

        // "Open with" launcher
        let mimes = ARCHIVE_MIMES.join(";");
        let desktop = format!(
            "[Desktop Entry]\nType=Application\nName=Universal 7-Zip GUI\nGenericName=Archive manager\nComment=GUI for the 7-Zip console program\nExec={} %F\nIcon=u7z\nTerminal=false\nCategories=Utility;Archiving;Compression;\nMimeType={};\nStartupWMClass=Universal 7-Zip GUI\n",
            q(exe),
            mimes
        );
        run(write(&dh.join("applications/u7z.desktop"), &desktop, false));

        // Dolphin / KDE service menus
        let mut kde = |file: &str, mime: &str, only_archives: bool| {
            let sel: Vec<&Item> = its.iter().filter(|i| i.archives_only == only_archives).collect();
            let mut s = format!("[Desktop Entry]\nType=Service\nMimeType={mime}\nActions={}\nX-KDE-Submenu=Universal 7-Zip GUI\nX-KDE-ServiceTypes=KonqPopupMenu/Plugin\nIcon=u7z\n", sel.iter().map(|i| format!("u7z_{}", i.id)).collect::<Vec<_>>().join(";") + ";");
            for i in sel {
                s.push_str(&format!("\n[Desktop Action u7z_{}]\nName={}\nExec={} {} %F\nIcon=u7z\n", i.id, i.label, q(exe), i.args));
            }
            run(write(&dh.join("kio/servicemenus").join(file), &s, true));
        };
        kde("u7z-archive.desktop", &format!("{};", mimes), true);
        kde("u7z-files.desktop", "all/allfiles;inode/directory;", false);

        // Nemo actions
        for i in &its {
            let sel = if i.archives_only { format!("Mimetypes={};\n", mimes) } else { "Extensions=any;\n".to_string() };
            let s = format!("[Nemo Action]\nName=Universal 7-Zip GUI: {}\nExec={} {} %F\nIcon-Name=u7z\nSelection=notnone\n{}", i.label, q(exe), i.args, sel);
            run(write(&dh.join("nemo/actions").join(format!("u7z-{}.nemo_action", i.id)), &s, false));
        }

        // Nautilus (GNOME Files) and Caja scripts
        let script = |var: &str, i: &Item| {
            format!("#!/bin/sh\n# Universal 7-Zip GUI\nIFS='\n'\nset -f\nexec {} {} ${{{}}}\n", sh_q(&exe.to_string_lossy()), i.args, var)
        };
        for i in &its {
            let name = format!("Universal 7-Zip GUI - {}", i.label.replace('/', "-"));
            run(write(&dh.join("nautilus/scripts").join(&name), &script("NAUTILUS_SCRIPT_SELECTED_FILE_PATHS", i), true));
            if config_home().join("caja").exists() || Path::new("/usr/bin/caja").exists() {
                run(write(&config_home().join("caja/scripts").join(&name), &script("CAJA_SCRIPT_SELECTED_FILE_PATHS", i), true));
            }
        }

        // Thunar custom actions
        let uca = config_home().join("Thunar/uca.xml");
        if uca.exists() || Path::new("/usr/bin/thunar").exists() {
            let mut cur = fs::read_to_string(&uca).unwrap_or_else(|_| "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<actions>\n</actions>\n".to_string());
            cur = strip_thunar(&cur);
            let mut block = String::new();
            for i in &its {
                let pat = if i.archives_only { ARCHIVE_EXTS.iter().map(|e| format!("*.{e}")).collect::<Vec<_>>().join(";") } else { "*".to_string() };
                let kinds = if i.archives_only { "<other-files/>" } else { "<directories/><audio-files/><image-files/><other-files/><text-files/><video-files/>" };
                block.push_str(&format!(
                    "<action>\n\t<icon>package-x-generic</icon>\n\t<name>Universal 7-Zip GUI: {}</name>\n\t<unique-id>u7z-{}</unique-id>\n\t<command>{} {} %F</command>\n\t<description></description>\n\t<patterns>{}</patterns>\n\t{}\n</action>\n",
                    xml_esc(&i.label), i.id, xml_esc(&q(exe)), i.args, pat, kinds
                ));
            }
            if let Some(pos) = cur.rfind("</actions>") {
                cur.insert_str(pos, &block);
                run(write(&uca, &cur, false));
            }
        }
        for tool in [("update-desktop-database", dh.join("applications")), ("gtk-update-icon-cache", dh.join("icons/hicolor")), ("kbuildsycoca6", PathBuf::new()), ("kbuildsycoca5", PathBuf::new())] {
            let mut c = std::process::Command::new(tool.0);
            if !tool.1.as_os_str().is_empty() {
                c.arg(&tool.1);
            }
            let _ = c.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
        }
        if done == 0 {
            return Err(errs.join("; "));
        }
        Ok(())
    }

    fn strip_thunar(s: &str) -> String {
        let mut out = String::new();
        let mut rest = s;
        while let Some(a) = rest.find("<action>") {
            let Some(b) = rest[a..].find("</action>") else { break };
            let end = a + b + "</action>".len();
            if rest[a..end].contains("<unique-id>u7z-") {
                out.push_str(rest[..a].trim_end_matches([' ', '\t']));
                rest = rest[end..].trim_start_matches('\n');
            } else {
                out.push_str(&rest[..end]);
                rest = &rest[end..];
            }
        }
        out.push_str(rest);
        out
    }

    pub fn uninstall() -> Result<(), String> {
        let dh = data_home();
        let mut files: Vec<PathBuf> = vec![dh.join("applications/u7z.desktop"), dh.join("kio/servicemenus/u7z-archive.desktop"), dh.join("kio/servicemenus/u7z-files.desktop"), dh.join("icons/hicolor/scalable/apps/u7z.svg")];
        for dir in [dh.join("nemo/actions"), dh.join("nautilus/scripts"), config_home().join("caja/scripts")] {
            if let Ok(rd) = fs::read_dir(&dir) {
                for e in rd.flatten() {
                    let n = e.file_name().to_string_lossy().to_string();
                    if n.starts_with("u7z-") || n.starts_with("Universal 7-Zip GUI - ") {
                        files.push(e.path());
                    }
                }
            }
        }
        for f in files {
            let _ = fs::remove_file(f);
        }
        let uca = config_home().join("Thunar/uca.xml");
        if let Ok(s) = fs::read_to_string(&uca) {
            let n = strip_thunar(&s);
            if n != s {
                let _ = fs::write(&uca, n);
            }
        }
        let _ = std::process::Command::new("update-desktop-database").arg(dh.join("applications")).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Windows: per-user registry entries (classic menu; on Windows 11 under "Show more options")
// ---------------------------------------------------------------------------------------------
/// (key, value name or "" for the default value, data)
#[allow(dead_code)]
fn registry_plan(exe: &std::path::Path, t: &I18n) -> Vec<(String, String, String)> {
    let base = r"HKCU\Software\Classes";
    let ex = exe.to_string_lossy().to_string();
    let its = items(t);
    let mut v: Vec<(String, String, String)> = vec![];
    for (menu, only_arch) in [("Universal7Zip.Archive", true), ("Universal7Zip.Files", false)] {
        // the archive menu holds everything, the files menu only the "add" entries
        let mut n = 0;
        for i in its.iter().filter(|i| only_arch || !i.archives_only) {
            n += 1;
            let key = format!(r"{base}\{menu}\shell\{n:02}{}", i.id);
            v.push((key.clone(), "MUIVerb".into(), i.label.clone()));
            v.push((format!(r"{key}\command"), String::new(), format!("\"{ex}\" {} %*", i.args)));
        }
    }
    let applies: String = ARCHIVE_EXTS.iter().map(|e| format!("System.FileExtension:=\".{e}\"")).collect::<Vec<_>>().join(" OR ");
    for (root, menu, extra) in [
        (format!(r"{base}\*\shell\Universal7Zip"), "Universal7Zip.Files", Some(format!("NOT ({applies})"))),
        (format!(r"{base}\Directory\shell\Universal7Zip"), "Universal7Zip.Files", None),
    ] {
        v.push((root.clone(), "MUIVerb".into(), "Universal 7-Zip GUI".into()));
        v.push((root.clone(), "ExtendedSubCommandsKey".into(), menu.into()));
        v.push((root.clone(), "MultiSelectModel".into(), "Player".into()));
        v.push((root.clone(), "Icon".into(), format!("\"{ex}\",0")));
        if let Some(a) = extra {
            v.push((root.clone(), "AppliesTo".into(), a));
        }
    }
    for e in ARCHIVE_EXTS {
        let root = format!(r"{base}\SystemFileAssociations\.{e}\shell\Universal7Zip");
        v.push((root.clone(), "MUIVerb".into(), "Universal 7-Zip GUI".into()));
        v.push((root.clone(), "ExtendedSubCommandsKey".into(), "Universal7Zip.Archive".into()));
        v.push((root.clone(), "MultiSelectModel".into(), "Player".into()));
        v.push((root.clone(), "Icon".into(), format!("\"{ex}\",0")));
    }
    v.push((format!(r"{base}\Applications\{}\shell\open\command", exe.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "u7z.exe".into())), String::new(), format!("\"{ex}\" \"%1\"")));
    v
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    fn reg(args: &[&str]) -> Result<(), String> {
        let o = Command::new("reg").args(args).creation_flags(0x0800_0000).output().map_err(|e| e.to_string())?;
        if o.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&o.stderr).trim().to_string()) }
    }
    pub fn install(exe: &std::path::Path, t: &I18n) -> Result<(), String> {
        for (key, name, data) in registry_plan(exe, t) {
            if name.is_empty() {
                reg(&["add", &key, "/ve", "/d", &data, "/f"])?;
            } else {
                reg(&["add", &key, "/v", &name, "/d", &data, "/f"])?;
            }
        }
        Ok(())
    }
    pub fn uninstall() -> Result<(), String> {
        let base = r"HKCU\Software\Classes";
        let mut keys = vec![format!(r"{base}\Universal7Zip.Archive"), format!(r"{base}\Universal7Zip.Files"), format!(r"{base}\*\shell\Universal7Zip"), format!(r"{base}\Directory\shell\Universal7Zip")];
        for e in ARCHIVE_EXTS {
            keys.push(format!(r"{base}\SystemFileAssociations\.{e}\shell\Universal7Zip"));
        }
        for k in keys {
            let _ = reg(&["delete", &k, "/f"]);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// macOS: Finder Quick Actions (Services)
// ---------------------------------------------------------------------------------------------
#[allow(dead_code)]
fn macos_workflow(name: &str, script: &str) -> (String, String) {
    let info = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>NSServices</key><array><dict><key>NSMenuItem</key><dict><key>default</key><string>{}</string></dict><key>NSMessage</key><string>runWorkflowAsService</string><key>NSRequiredContext</key><dict><key>NSApplicationIdentifier</key><string>com.apple.finder</string></dict><key>NSSendFileTypes</key><array><string>public.item</string></array></dict></array></dict></plist>\n",
        xml_esc(name)
    );
    let wflow = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n<key>AMApplicationBuild</key><string>523</string><key>AMApplicationVersion</key><string>2.10</string><key>AMDocumentVersion</key><string>2</string>\n<key>actions</key><array><dict><key>action</key><dict>\n<key>AMAccepts</key><dict><key>Container</key><string>List</string><key>Optional</key><true/><key>Types</key><array><string>com.apple.cocoa.string</string></array></dict>\n<key>AMActionVersion</key><string>2.0.3</string><key>AMApplication</key><array><string>Automator</string></array>\n<key>AMParameterProperties</key><dict><key>COMMAND_STRING</key><dict/><key>CheckedForUserDefaultShell</key><dict/><key>inputMethod</key><dict/><key>shell</key><dict/><key>source</key><dict/></dict>\n<key>AMProvides</key><dict><key>Container</key><string>List</string><key>Types</key><array><string>com.apple.cocoa.string</string></array></dict>\n<key>ActionBundlePath</key><string>/System/Library/Automator/Run Shell Script.action</string><key>ActionName</key><string>Run Shell Script</string>\n<key>ActionParameters</key><dict><key>COMMAND_STRING</key><string>{}</string><key>CheckedForUserDefaultShell</key><true/><key>inputMethod</key><integer>1</integer><key>shell</key><string>/bin/bash</string><key>source</key><string></string></dict>\n<key>BundleIdentifier</key><string>com.apple.RunShellScript</string><key>CFBundleVersion</key><string>2.0.3</string><key>CanShowSelectedItemsWhenRun</key><false/><key>CanShowWhenRun</key><true/><key>Category</key><array><string>AMCategoryUtilities</string></array><key>Class Name</key><string>RunShellScriptAction</string><key>InputUUID</key><string>5C6B8A6E-0000-4000-8000-000000000001</string><key>OutputUUID</key><string>5C6B8A6E-0000-4000-8000-000000000002</string><key>UUID</key><string>5C6B8A6E-0000-4000-8000-000000000003</string>\n<key>arguments</key><dict/></dict></dict></array>\n<key>connectors</key><dict/>\n<key>workflowMetaData</key><dict><key>serviceInputTypeIdentifier</key><string>com.apple.Automator.fileSystemObject</string><key>serviceOutputTypeIdentifier</key><string>com.apple.Automator.nothing</string><key>serviceProcessesInput</key><integer>0</integer><key>workflowTypeIdentifier</key><string>com.apple.Automator.servicesMenu</string></dict>\n</dict></plist>\n",
        xml_esc(script)
    );
    (info, wflow)
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use std::fs;
    fn dir() -> PathBuf {
        std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join("Library/Services")
    }
    pub fn install(exe: &std::path::Path, t: &I18n) -> Result<(), String> {
        let ex = exe.to_string_lossy().replace('\'', "'\\''");
        for i in items(t) {
            let name = format!("Universal 7-Zip GUI - {}", i.label.replace('/', "-"));
            let script = format!("'{ex}' {} \"$@\"", i.args);
            let (info, wflow) = macos_workflow(&name, &script);
            let c = dir().join(format!("{name}.workflow")).join("Contents");
            fs::create_dir_all(&c).map_err(|e| e.to_string())?;
            fs::write(c.join("Info.plist"), info).map_err(|e| e.to_string())?;
            fs::write(c.join("document.wflow"), wflow).map_err(|e| e.to_string())?;
        }
        let _ = std::process::Command::new("/System/Library/CoreServices/pbs").arg("-flush").status();
        Ok(())
    }
    pub fn uninstall() -> Result<(), String> {
        if let Ok(rd) = fs::read_dir(dir()) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().starts_with("Universal 7-Zip GUI - ") {
                    let _ = fs::remove_dir_all(e.path());
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plan_has_commands() {
        let t = I18n::load("en");
        let p = registry_plan(std::path::Path::new(r"C:\Apps\u7zgui.exe"), &t);
        assert!(p.iter().any(|(k, _, d)| k.ends_with(r"01extract\command") && d.contains("extract %*")));
        assert!(p.iter().any(|(k, n, _)| k.contains(r"\.7z\shell") && n == "ExtendedSubCommandsKey"));
        let (i, w) = macos_workflow("A & B", "echo 'x' \"$@\"");
        assert!(i.contains("A &amp; B") && w.contains("&quot;$@&quot;"));
    }
}
