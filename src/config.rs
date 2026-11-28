//! Config file in bashtop format (`key="value"`), at ~/.config/rbtop/rbtop.cfg.

use std::path::PathBuf;

pub const SORTINGS: [&str; 8] = ["pid", "program", "arguments", "threads", "user", "memory", "cpu lazy", "cpu responsive"];

#[derive(Clone)]
pub struct Config {
    pub color_theme: String,
    pub update_ms: u64,
    pub proc_sorting: String,
    pub proc_reversed: bool,
    pub proc_tree: bool,
    pub check_temp: bool,
    pub draw_clock: String,
    pub background_update: bool,
    pub custom_cpu_name: String,
    pub proc_gradient: bool,
    pub proc_per_core: bool,
    pub disks_filter: String,
    pub hires_graphs: bool,
    // ---- additions (GitHub feature requests)
    pub mem_graphs: bool,
    pub show_swap: bool,
    pub show_battery: bool,
    pub show_gpu: bool,
    pub show_containers: bool,
    pub net_device: String,
    pub mouse: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            color_theme: "Default".into(),
            update_ms: 2500,
            proc_sorting: "cpu lazy".into(),
            proc_reversed: false,
            proc_tree: false,
            check_temp: true,
            draw_clock: "%X".into(),
            background_update: true,
            custom_cpu_name: String::new(),
            proc_gradient: true,
            proc_per_core: false,
            disks_filter: String::new(),
            hires_graphs: false,
            mem_graphs: false,
            show_swap: true,
            show_battery: true,
            show_gpu: true,
            show_containers: true,
            net_device: String::new(),
            mouse: true,
        }
    }
}

pub enum Kind {
    Bool,
    Int,
    Str,
    Choice(fn() -> Vec<String>),
}

pub struct OptDef {
    pub key: &'static str,
    pub kind: Kind,
    pub desc: &'static [&'static str],
}

fn sortings() -> Vec<String> {
    SORTINGS.iter().map(|s| s.to_string()).collect()
}
fn themes() -> Vec<String> {
    crate::theme::available()
}

pub const OPTIONS: &[OptDef] = &[
    OptDef { key: "color_theme", kind: Kind::Choice(themes), desc: &["Set color theme.", "Looks in ~/.config/rbtop/themes and", "the bashtop/bpytop/btop theme folders.", "Use LEFT/RIGHT to cycle."] },
    OptDef { key: "update_ms", kind: Kind::Int, desc: &["Update time in milliseconds.", "Recommended 2000 ms or above for", "better sample times for graphs.", "Min 100, max 86399900."] },
    OptDef { key: "proc_sorting", kind: Kind::Choice(sortings), desc: &["Processes sorting.", "\"cpu lazy\" smooths cpu usage over time,", "\"cpu responsive\" uses the last sample."] },
    OptDef { key: "proc_reversed", kind: Kind::Bool, desc: &["Reverse sorting order."] },
    OptDef { key: "proc_tree", kind: Kind::Bool, desc: &["Processes tree view."] },
    OptDef { key: "check_temp", kind: Kind::Bool, desc: &["Check cpu temperature.", "Reads hwmon: coretemp, k10temp,", "zenpower, cpu_thermal, acpitz."] },
    OptDef { key: "draw_clock", kind: Kind::Str, desc: &["Draw a clock at top of screen.", "Formatting according to strftime,", "empty string to disable."] },
    OptDef { key: "background_update", kind: Kind::Bool, desc: &["Update main ui when menus are showing.", "Set to false to pause collection", "while a menu is open."] },
    OptDef { key: "custom_cpu_name", kind: Kind::Str, desc: &["Custom cpu model name in cpu box.", "Empty string to disable."] },
    OptDef { key: "proc_gradient", kind: Kind::Bool, desc: &["Show color gradient in process list."] },
    OptDef { key: "proc_per_core", kind: Kind::Bool, desc: &["Process usage per core.", "If true, one fully used core = 100%."] },
    OptDef { key: "disks_filter", kind: Kind::Str, desc: &["Filter for shown disks.", "Mountpoint names, \"root\" replaces \"/\".", "Separate with space.", "Prefix \"exclude=\" to exclude instead."] },
    OptDef { key: "hires_graphs", kind: Kind::Bool, desc: &["Double horizontal graph resolution", "(two samples per braille cell)."] },
    OptDef { key: "mem_graphs", kind: Kind::Bool, desc: &["Show graphs instead of meters", "for memory values.", "(bashtop issue #73)"] },
    OptDef { key: "show_swap", kind: Kind::Bool, desc: &["Show swap (and zram) in mem box.", "(bashtop issue #119)"] },
    OptDef { key: "show_battery", kind: Kind::Bool, desc: &["Show battery status in cpu box", "if a battery is found.", "(bashtop issue #199)"] },
    OptDef { key: "show_gpu", kind: Kind::Bool, desc: &["Show GPU load/vram/temp in cpu box.", "AMD via sysfs, NVIDIA via a single", "long-running nvidia-smi.", "(bashtop issues #26, #221)"] },
    OptDef { key: "show_containers", kind: Kind::Bool, desc: &["Detect containers per process", "(Docker, Podman, LXC/Proxmox CT, VMs).", "Shown in detail view, filterable.", "(bashtop issue #97)"] },
    OptDef { key: "net_device", kind: Kind::Str, desc: &["Network device to show at start.", "Empty = interface of default route."] },
    OptDef { key: "mouse", kind: Kind::Bool, desc: &["Mouse support: scroll and click in", "process list, clickable buttons.", "Takes effect after restart."] },
];

pub fn config_dir() -> Option<PathBuf> {
    if let Ok(x) = std::env::var("XDG_CONFIG_HOME") {
        if !x.is_empty() {
            return Some(PathBuf::from(x).join("rbtop"));
        }
    }
    std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".config").join("rbtop"))
}

pub fn config_file() -> Option<PathBuf> {
    config_dir().map(|d| d.join("rbtop.cfg"))
}

fn b(s: &str) -> bool {
    matches!(s.to_ascii_lowercase().as_str(), "true" | "1" | "yes" | "on")
}

impl Config {
    pub fn get(&self, key: &str) -> String {
        match key {
            "color_theme" => self.color_theme.clone(),
            "update_ms" => self.update_ms.to_string(),
            "proc_sorting" => self.proc_sorting.clone(),
            "proc_reversed" => self.proc_reversed.to_string(),
            "proc_tree" => self.proc_tree.to_string(),
            "check_temp" => self.check_temp.to_string(),
            "draw_clock" => self.draw_clock.clone(),
            "background_update" => self.background_update.to_string(),
            "custom_cpu_name" => self.custom_cpu_name.clone(),
            "proc_gradient" => self.proc_gradient.to_string(),
            "proc_per_core" => self.proc_per_core.to_string(),
            "disks_filter" => self.disks_filter.clone(),
            "hires_graphs" => self.hires_graphs.to_string(),
            "mem_graphs" => self.mem_graphs.to_string(),
            "show_swap" => self.show_swap.to_string(),
            "show_battery" => self.show_battery.to_string(),
            "show_gpu" => self.show_gpu.to_string(),
            "show_containers" => self.show_containers.to_string(),
            "net_device" => self.net_device.clone(),
            "mouse" => self.mouse.to_string(),
            _ => String::new(),
        }
    }

    pub fn set(&mut self, key: &str, v: &str) {
        match key {
            "color_theme" => self.color_theme = v.into(),
            "update_ms" => {
                if let Ok(n) = v.parse::<u64>() {
                    self.update_ms = n.clamp(100, 86_399_900)
                }
            }
            "proc_sorting" => {
                if SORTINGS.contains(&v) {
                    self.proc_sorting = v.into()
                } else if v == "tree" {
                    self.proc_tree = true
                }
            }
            "proc_reversed" => self.proc_reversed = b(v),
            "proc_tree" => self.proc_tree = b(v),
            "check_temp" => self.check_temp = b(v),
            "draw_clock" => self.draw_clock = v.into(),
            "background_update" => self.background_update = b(v),
            "custom_cpu_name" => self.custom_cpu_name = v.into(),
            "proc_gradient" => self.proc_gradient = b(v),
            "proc_per_core" => self.proc_per_core = b(v),
            "disks_filter" => self.disks_filter = v.into(),
            "hires_graphs" => self.hires_graphs = b(v),
            "mem_graphs" => self.mem_graphs = b(v),
            "show_swap" => self.show_swap = b(v),
            "show_battery" => self.show_battery = b(v),
            "show_gpu" => self.show_gpu = b(v),
            "show_containers" => self.show_containers = b(v),
            "net_device" => self.net_device = v.into(),
            "mouse" => self.mouse = b(v),
            _ => {}
        }
    }

    pub fn load() -> Config {
        let mut c = Config::default();
        // Fall back to an existing bashtop config for a seamless switch.
        let mut paths = vec![];
        if let Some(p) = config_file() {
            paths.push(p);
        }
        if let Ok(h) = std::env::var("HOME") {
            paths.push(PathBuf::from(h).join(".config/bashtop/bashtop.cfg"));
        }
        for p in paths {
            if let Ok(s) = std::fs::read_to_string(&p) {
                for line in s.lines() {
                    let l = line.trim();
                    if l.starts_with('#') {
                        continue;
                    }
                    if let Some((k, v)) = l.split_once('=') {
                        // strip one pair of quotes only, so a value may contain '"'
                        let v = v.trim();
                        let v = v.strip_prefix('"').and_then(|x| x.strip_suffix('"')).unwrap_or(v);
                        c.set(k.trim(), v);
                    }
                }
                break;
            }
        }
        c
    }

    pub fn save(&self) {
        let Some(path) = config_file() else { return };
        if let Some(d) = path.parent() {
            let _ = std::fs::create_dir_all(d);
            let _ = std::fs::create_dir_all(d.join("themes"));
        }
        let mut s = format!("#? Config file for rbtop v. {}\n", env!("CARGO_PKG_VERSION"));
        for o in OPTIONS {
            s.push('\n');
            for d in o.desc {
                s.push_str("#* ");
                s.push_str(d);
                s.push('\n');
            }
            s.push_str(&format!("{}=\"{}\"\n", o.key, self.get(o.key)));
        }
        // write + rename: a kill or full disk mid-write never leaves a truncated config
        let tmp = path.with_extension("cfg.tmp");
        if std::fs::write(&tmp, s).is_ok() && std::fs::rename(&tmp, &path).is_ok() {
            fix_sudo_owner(&path);
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

/// `sudo rbtop` keeps HOME on macOS (and with `sudo -E` on Linux), so the
/// config would end up owned by root inside the user's home and later normal
/// runs could no longer save it. Hand what we created back to the sudo user,
/// but only when ~/.config itself belongs to that user (never root's own home).
fn fix_sudo_owner(file: &std::path::Path) {
    use std::os::unix::fs::MetadataExt;
    if unsafe { libc::geteuid() } != 0 {
        return;
    }
    let id = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<u32>().ok()).filter(|&v| v != 0);
    let (Some(uid), Some(gid)) = (id("SUDO_UID"), id("SUDO_GID")) else { return };
    let Some(dir) = file.parent() else { return };
    let Some(base) = dir.parent() else { return };
    if std::fs::metadata(base).map(|m| m.uid()).ok() != Some(uid) {
        return;
    }
    for p in [dir.to_path_buf(), dir.join("themes"), file.to_path_buf()] {
        // lchown semantics: never follow a symlink planted in place of the file
        if std::fs::symlink_metadata(&p).is_ok_and(|m| m.uid() == 0 && !m.file_type().is_symlink()) {
            let _ = std::os::unix::fs::lchown(&p, Some(uid), Some(gid));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn save_and_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!("rbtop-cfgtest-{}", std::process::id()));
        // config_dir() reads XDG_CONFIG_HOME; tests in this binary run in threads,
        // and no other test touches the config, so setting it here is fine
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        let mut c = Config::default();
        c.draw_clock = "%H:%M \"x\"".into();
        c.update_ms = 1234;
        c.save();
        let l = Config::load();
        assert_eq!(l.draw_clock, "%H:%M \"x\"");
        assert_eq!(l.update_ms, 1234);
        assert!(!dir.join("rbtop/rbtop.cfg.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
