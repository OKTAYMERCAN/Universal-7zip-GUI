use std::path::PathBuf;

#[derive(Clone, Debug, Default)]
pub struct Settings {
    pub theme: String, // auto | light | dark | oled
    pub lang: String,  // "" = follow the system
    pub last_dir: String,
}

fn path() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    }?;
    Some(base.join("u7zgui").join("settings.json"))
}

impl Settings {
    pub fn load() -> Settings {
        let mut s = Settings { theme: "auto".into(), ..Default::default() };
        if let Some(p) = path() {
            if let Ok(t) = std::fs::read_to_string(p) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
                    s.theme = v["theme"].as_str().unwrap_or("auto").to_string();
                    s.lang = v["lang"].as_str().unwrap_or("").to_string();
                    s.last_dir = v["last_dir"].as_str().unwrap_or("").to_string();
                }
            }
        }
        s
    }
    pub fn save(&self) {
        if let Some(p) = path() {
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let v = serde_json::json!({"theme": self.theme, "lang": self.lang, "last_dir": self.last_dir});
            let _ = std::fs::write(p, v.to_string());
        }
    }
}
