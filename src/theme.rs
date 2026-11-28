//! Theme handling. Reads bashtop/bpytop/btop `.theme` files unchanged
//! (`theme[key]="#rrggbb"`, `"#gg"` greyscale or `"r g b"` decimal).

use crate::term::{Color, DEFAULT};
use std::path::PathBuf;

pub type Gradient = [Color; 101];

pub struct Theme {
    pub main_bg: Color,
    pub main_fg: Color,
    pub title: Color,
    pub hi_fg: Color,
    pub selected_bg: Color,
    pub selected_fg: Color,
    pub inactive_fg: Color,
    pub proc_misc: Color,
    pub cpu_box: Color,
    pub mem_box: Color,
    pub net_box: Color,
    pub proc_box: Color,
    pub div_line: Color,
    pub temp: Gradient,
    pub cpu: Gradient,
    pub free: Gradient,
    pub cached: Gradient,
    pub available: Gradient,
    pub used: Gradient,
    pub download: Gradient,
    pub upload: Gradient,
    /// Process list fade (main_fg -> inactive_fg) for proc_gradient.
    pub proc_fade: Gradient,
}

const DEFAULTS: &[(&str, &str)] = &[
    ("main_bg", ""),
    ("main_fg", "#cc"),
    ("title", "#ee"),
    ("hi_fg", "#90"),
    ("selected_bg", "#7e2626"),
    ("selected_fg", "#ee"),
    ("inactive_fg", "#40"),
    ("proc_misc", "#0de756"),
    ("cpu_box", "#3d7b46"),
    ("mem_box", "#8a882e"),
    ("net_box", "#423ba5"),
    ("proc_box", "#923535"),
    ("div_line", "#30"),
    ("temp_start", "#4897d4"),
    ("temp_mid", "#5474e8"),
    ("temp_end", "#ff40b6"),
    ("cpu_start", "#50f095"),
    ("cpu_mid", "#f2e266"),
    ("cpu_end", "#fa1e1e"),
    ("free_start", "#223014"),
    ("free_mid", "#b5e685"),
    ("free_end", "#dcff85"),
    ("cached_start", "#0b1a29"),
    ("cached_mid", "#74e6fc"),
    ("cached_end", "#26c5ff"),
    ("available_start", "#292107"),
    ("available_mid", "#ffd77a"),
    ("available_end", "#ffb814"),
    ("used_start", "#3b1f1c"),
    ("used_mid", "#d9626d"),
    ("used_end", "#ff4769"),
    ("download_start", "#231a63"),
    ("download_mid", "#4f43a3"),
    ("download_end", "#b0a9de"),
    ("upload_start", "#510554"),
    ("upload_mid", "#7d4180"),
    ("upload_end", "#dcafde"),
];

pub fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(h) = s.strip_prefix('#') {
        return match h.len() {
            2 => u32::from_str_radix(h, 16).ok().map(|g| (g << 16) | (g << 8) | g),
            6 => u32::from_str_radix(h, 16).ok(),
            _ => None,
        };
    }
    let p: Vec<u32> = s.split_whitespace().filter_map(|x| x.parse().ok()).collect();
    if p.len() == 3 {
        return Some(((p[0] & 0xff) << 16) | ((p[1] & 0xff) << 8) | (p[2] & 0xff));
    }
    None
}

fn lerp(a: Color, b: Color, t: f32) -> Color {
    let f = |sh: u32| {
        let x = ((a >> sh) & 0xff) as f32;
        let y = ((b >> sh) & 0xff) as f32;
        ((x + (y - x) * t).round() as u32) & 0xff
    };
    (f(16) << 16) | (f(8) << 8) | f(0)
}

pub fn gradient(start: Color, mid: Option<Color>, end: Color) -> Gradient {
    let mut g = [0; 101];
    for (i, c) in g.iter_mut().enumerate() {
        *c = match mid {
            Some(m) if i <= 50 => lerp(start, m, i as f32 / 50.0),
            Some(m) => lerp(m, end, (i - 50) as f32 / 50.0),
            None => lerp(start, end, i as f32 / 100.0),
        };
    }
    g
}

pub fn theme_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(c) = crate::config::config_dir() {
        v.push(c.join("themes"));
        v.push(c.join("user_themes"));
    }
    if let Ok(h) = std::env::var("HOME") {
        for app in ["bashtop", "bpytop", "btop"] {
            v.push(PathBuf::from(&h).join(".config").join(app).join("themes"));
            v.push(PathBuf::from(&h).join(".config").join(app).join("user_themes"));
        }
    }
    for p in ["/usr/local/share/rbtop/themes", "/usr/share/rbtop/themes", "/usr/local/share/bashtop/themes",
        "/usr/share/bashtop/themes", "/usr/local/share/bpytop/themes", "/usr/share/bpytop/themes",
        "/usr/local/share/btop/themes", "/usr/share/btop/themes",
        // Homebrew on Apple Silicon
        "/opt/homebrew/share/rbtop/themes", "/opt/homebrew/share/bpytop/themes", "/opt/homebrew/share/btop/themes"] {
        v.push(PathBuf::from(p));
    }
    // themes/ next to the binary (portable install), and <prefix>/share/rbtop/themes
    // for <prefix>/bin/rbtop: Homebrew's Cellar and Linuxbrew, .deb, make install
    // with any PREFIX (current_exe resolves the bin/ symlink)
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            v.push(d.join("themes"));
            if let Some(prefix) = d.parent() {
                v.push(prefix.join("share/rbtop/themes"));
            }
        }
    }
    v
}

/// All theme names found, "Default" first, de-duplicated.
pub fn available() -> Vec<String> {
    let mut names = vec!["Default".to_string()];
    for d in theme_dirs() {
        if let Ok(rd) = std::fs::read_dir(&d) {
            let mut found: Vec<String> = rd
                .flatten()
                .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".theme")).map(String::from))
                .collect();
            found.sort();
            for n in found {
                if !names.contains(&n) {
                    names.push(n);
                }
            }
        }
    }
    names
}

fn find(name: &str) -> Option<String> {
    let base = name.rsplit('/').next().unwrap_or(name).trim_end_matches(".theme");
    if std::path::Path::new(name).is_file() {
        return std::fs::read_to_string(name).ok();
    }
    for d in theme_dirs() {
        let p = d.join(format!("{base}.theme"));
        if let Ok(s) = std::fs::read_to_string(&p) {
            return Some(s);
        }
    }
    None
}

impl Theme {
    pub fn load(name: &str) -> Theme {
        let mut kv: std::collections::HashMap<String, String> = DEFAULTS.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let mut mids_set = std::collections::HashSet::new();
        if name != "Default" && !name.is_empty() {
            if let Some(src) = find(name) {
                for line in src.lines() {
                    let l = line.trim();
                    if let Some(rest) = l.strip_prefix("theme[") {
                        if let Some((k, v)) = rest.split_once("]=") {
                            let v = v.trim().trim_matches('"').trim_matches('\'');
                            if k.ends_with("_mid") {
                                mids_set.insert(k.to_string());
                            }
                            kv.insert(k.to_string(), v.to_string());
                        }
                    }
                }
                // A theme that sets start/end but no mid means "no mid".
                for g in ["temp", "cpu", "free", "cached", "available", "used", "download", "upload"] {
                    let mk = format!("{g}_mid");
                    if !mids_set.contains(&mk) {
                        kv.insert(mk, String::new());
                    }
                }
            }
        }
        let c = |k: &str| kv.get(k).and_then(|v| parse_color(v));
        let cd = |k: &str| c(k).unwrap_or(DEFAULT);
        let gr = |p: &str| {
            let s = c(&format!("{p}_start")).unwrap_or(0x808080);
            let e = c(&format!("{p}_end")).unwrap_or(s);
            gradient(s, c(&format!("{p}_mid")), e)
        };
        let main_fg = c("main_fg").unwrap_or(0xcccccc);
        let inactive = c("inactive_fg").unwrap_or(0x404040);
        Theme {
            main_bg: cd("main_bg"),
            main_fg,
            title: c("title").unwrap_or(0xeeeeee),
            hi_fg: c("hi_fg").unwrap_or(0x909090),
            selected_bg: c("selected_bg").unwrap_or(0x7e2626),
            selected_fg: c("selected_fg").unwrap_or(0xeeeeee),
            inactive_fg: inactive,
            proc_misc: c("proc_misc").unwrap_or(0x0de756),
            cpu_box: c("cpu_box").unwrap_or(0x3d7b46),
            mem_box: c("mem_box").unwrap_or(0x8a882e),
            net_box: c("net_box").unwrap_or(0x423ba5),
            proc_box: c("proc_box").unwrap_or(0x923535),
            div_line: c("div_line").unwrap_or(0x303030),
            temp: gr("temp"),
            cpu: gr("cpu"),
            free: gr("free"),
            cached: gr("cached"),
            available: gr("available"),
            used: gr("used"),
            download: gr("download"),
            upload: gr("upload"),
            proc_fade: gradient(main_fg, None, lerp(main_fg, inactive, 0.75)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn colors() {
        assert_eq!(parse_color("#cc"), Some(0xcccccc));
        assert_eq!(parse_color("#3d7b46"), Some(0x3d7b46));
        assert_eq!(parse_color("255 0 16"), Some(0xff0010));
        assert_eq!(parse_color(""), None);
    }
    #[test]
    fn bundled_themes_parse() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/themes");
        let mut n = 0;
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            let t = Theme::load(p.to_str().unwrap());
            assert_ne!(t.main_fg, 0x1000000);
            n += 1;
        }
        assert!(n >= 9);
    }
}
