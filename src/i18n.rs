//! Translations. The dictionary lives in assets/i18n.json (generated from the web version's
//! i18n.js plus tools/i18n_extra.js) and is embedded in the executable.
use crate::core::Err;
use std::collections::HashMap;

pub struct I18n {
    pub langs: Vec<(String, String)>,
    d: HashMap<String, HashMap<String, String>>,
    pub lang: String,
}

impl I18n {
    pub fn load(lang: &str) -> I18n {
        let v: serde_json::Value = serde_json::from_str(include_str!("../assets/i18n.json")).expect("i18n.json");
        let langs = v["langs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| (p[0].as_str().unwrap().to_string(), p[1].as_str().unwrap().to_string()))
            .collect();
        let mut d = HashMap::new();
        for (l, m) in v["d"].as_object().unwrap() {
            let mut mm = HashMap::new();
            for (k, s) in m.as_object().unwrap() {
                mm.insert(k.clone(), s.as_str().unwrap_or("").to_string());
            }
            d.insert(l.clone(), mm);
        }
        let mut i = I18n { langs, d, lang: "en".into() };
        i.set_lang(lang);
        i
    }
    pub fn set_lang(&mut self, l: &str) {
        self.lang = if self.d.contains_key(l) { l.to_string() } else { "en".to_string() };
    }
    pub fn t(&self, key: &str) -> String {
        self.d
            .get(&self.lang)
            .and_then(|m| m.get(key))
            .or_else(|| self.d.get("en").and_then(|m| m.get(key)))
            .cloned()
            .unwrap_or_else(|| key.to_string())
    }
    pub fn tf(&self, key: &str, args: &[&str]) -> String {
        let mut s = self.t(key);
        for (i, a) in args.iter().enumerate() {
            s = s.replace(&format!("{{{i}}}"), a);
        }
        s
    }
    pub fn err(&self, e: &Err) -> String {
        let k = format!("e.{}", e.code);
        let has = self.d.get("en").map(|m| m.contains_key(&k)).unwrap_or(false);
        if has {
            self.tf(&k, &[e.arg.as_deref().unwrap_or("")])
        } else {
            format!("{}{}", e.code, e.arg.as_ref().map(|a| format!(": {a}")).unwrap_or_default())
        }
    }
    pub fn prop(&self, k: &str) -> String {
        let key = format!("k.{k}");
        let loc = self.d.get(&self.lang).and_then(|m| m.get(&key)).cloned();
        loc.or_else(|| self.d.get("en").and_then(|m| m.get(&key)).cloned()).unwrap_or_else(|| k.to_string())
    }
}

/// Best guess of the user's UI language, two letters, e.g. "tr".
pub fn system_lang() -> String {
    for k in ["LC_ALL", "LC_MESSAGES", "LANGUAGE", "LANG"] {
        if let Ok(v) = std::env::var(k) {
            let l: String = v.chars().take(2).collect::<String>().to_lowercase();
            if l.len() == 2 && l != "c" && l.chars().all(|c| c.is_ascii_alphabetic()) {
                return l;
            }
        }
    }
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetUserDefaultLocaleName(buf: *mut u16, len: i32) -> i32;
        }
        let mut buf = [0u16; 85];
        let n = unsafe { GetUserDefaultLocaleName(buf.as_mut_ptr(), 85) };
        if n > 2 {
            return String::from_utf16_lossy(&buf[..2]).to_lowercase();
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(o) = std::process::Command::new("defaults").args(["read", "-g", "AppleLanguages"]).output() {
            let t = String::from_utf8_lossy(&o.stdout).to_string();
            if let Some(i) = t.find('"') {
                return t[i + 1..].chars().take(2).collect::<String>().to_lowercase();
            }
        }
    }
    "en".into()
}
