//! rbtop — bashtop rewritten in Rust (Linux and macOS).
//! Same layout, same keys, same theme/config format; a fraction of the CPU.

mod common;
#[cfg(target_os = "linux")]
mod collect;
#[cfg(target_os = "macos")]
#[path = "collect_macos.rs"]
mod collect;
#[cfg(target_os = "macos")]
mod iokit;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("rbtop supports Linux and macOS only");
mod config;
mod draw;
mod procs;
mod term;
mod theme;

use collect::{Battery, Cpu, Disks, Gpus, Mem, Net};
use config::{Config, Kind, OPTIONS, SORTINGS};
use draw::*;
use procs::Procs;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use term::{Color, Key, Screen};
use theme::Theme;

#[derive(Clone, Copy, Default)]
struct Rect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}


struct Layout {
    cpu: Rect,
    mem: Rect,
    net: Rect,
    procs: Rect,
    detail: Option<Rect>,
    cin: Rect,
    cols: i32,
    colw: i32,
    graphw: i32,
    core_rows: i32,
    core_temp: bool,
}

#[derive(Clone)]
enum Mode {
    Normal,
    Menu { sel: usize },
    Options { sel: usize, edit: Option<String> },
    Help { page: usize },
    Signal { sig: i32, verb: &'static str, signame: &'static str, pid: i32, name: String, sel: usize, result: Option<Vec<String>> },
}

struct Button {
    x: i32,
    w: i32,
    y: i32,
    key: Key,
}

struct App {
    cfg: Config,
    th: Theme,
    scr: Screen,
    cpu: Cpu,
    mem: Mem,
    disks: Disks,
    net: Net,
    procs: Procs,
    bat: Battery,
    gpu: Gpus,
    sel: Option<usize>,
    start: usize,
    page: usize,
    filter: String,
    filter_input: bool,
    rows_dirty: bool,
    mode: Mode,
    buttons: Vec<Button>,
    quit: bool,
    next_update: Instant,
    debug: bool,
    draw_us: u128,
    collect_us: u128,
    status: Option<(String, Instant)>,
}

fn round_pct(total: i32, p: i32) -> i32 {
    ((total as f64) * (p as f64) / 100.0).round() as i32
}

const HELP: &[(&str, &str)] = &[
    ("(Esc, M, m)", "Shows main menu."),
    ("(F2, O, o)", "Shows options."),
    ("(F1, H, h)", "Shows this window."),
    ("(Ctrl-C, Q, q)", "Quits program."),
    ("(+, A, a) (-, S, s)", "Add/Subtract 100ms to/from update timer."),
    ("(Up) (Down)", "Select in process list."),
    ("(Enter)", "Show detailed information for selected process."),
    ("(Pg Up) (Pg Down)", "Jump 1 page in process list."),
    ("(Home) (End)", "Jump to first or last page in process list."),
    ("(Left) (Right)", "Select previous/next sorting column."),
    ("(b, B) (n, N)", "Select previous/next network device."),
    ("(E, e)", "Toggle processes tree view"),
    ("(R, r)", "Reverse sorting order in processes box."),
    ("(F, f)", "Input a string to filter processes with."),
    ("(C, c)", "Clear any entered filter."),
    ("Selected (T, t)", "Terminate selected process with SIGTERM - 15."),
    ("Selected (K, k)", "Kill selected process with SIGKILL - 9."),
    ("Selected (I, i)", "Interrupt selected process with SIGINT - 2."),
    ("", ""),
    ("New in rbtop:", ""),
    ("Selected (P, p)", "Pause/resume with SIGSTOP/SIGCONT (#185)."),
    ("(Z, z)", "Reset network download/upload totals (#63)."),
    ("(Ctrl-Z)", "Suspend rbtop to shell (fg to resume)."),
    ("Mouse", "Scroll/click in process list, click buttons."),
    ("Mouse header", "Click a column title to sort, again to reverse."),
    ("Filter", "Matches pid, name, args, user, container."),
    ("", ""),
    ("", "Rust rewrite of bashtop by aristocratos:"),
    ("", "https://github.com/aristocratos/bashtop"),
];

impl App {
    fn new(debug: bool) -> App {
        let cfg = Config::load();
        let th = Theme::load(&cfg.color_theme);
        let (w, h) = term::size();
        let procs = Procs::new();
        let mut a = App {
            th,
            scr: Screen::new(w, h),
            cpu: Cpu::new(&cfg.custom_cpu_name),
            mem: Mem::new(),
            disks: Disks::new(&cfg.disks_filter),
            net: Net::new(&cfg.net_device),
            procs,
            bat: Battery::new(),
            gpu: Gpus::new(cfg.show_gpu, cfg.update_ms),
            sel: None,
            start: 0,
            page: 10,
            filter: String::new(),
            filter_input: false,
            rows_dirty: true,
            mode: Mode::Normal,
            buttons: Vec::new(),
            quit: false,
            next_update: Instant::now(),
            debug,
            draw_us: 0,
            collect_us: 0,
            status: None,
            cfg,
        };
        // Prime process cpu deltas so the first screen already has values.
        a.procs.collect(a.cpu.threads, a.mem.total, a.cfg.proc_per_core, a.cfg.show_containers);
        std::thread::sleep(Duration::from_millis(100));
        a.collect();
        a
    }

    fn collect(&mut self) {
        let t = Instant::now();
        self.cpu.collect(self.cfg.check_temp);
        self.mem.collect();
        self.disks.collect();
        self.net.collect();
        self.procs.collect(self.cpu.threads, self.mem.total, self.cfg.proc_per_core, self.cfg.show_containers);
        if self.cfg.show_battery {
            self.bat.collect();
        }
        if self.cfg.show_gpu {
            self.gpu.collect();
        }
        self.rows_dirty = true;
        self.collect_us = t.elapsed().as_micros();
    }

    fn rebuild_rows(&mut self) {
        if self.rows_dirty {
            // The selection follows its process, not its row: after re-sorting,
            // the highlighted (and kill-dialog) target must stay the same pid.
            let sel_pid = self.sel.and_then(|s| self.procs.rows.get(s)).map(|r| r.pid);
            let sel_off = self.sel.map(|s| s.saturating_sub(self.start));
            self.procs.build_rows(&self.cfg.proc_sorting, self.cfg.proc_reversed, self.cfg.proc_tree, &self.filter);
            self.rows_dirty = false;
            let n = self.procs.rows.len();
            if let Some(s) = self.sel {
                if n == 0 {
                    self.sel = None;
                } else if let Some(i) = sel_pid.and_then(|p| self.procs.rows.iter().position(|r| r.pid == p)) {
                    self.sel = Some(i);
                    // keep the selected row at the same screen line if possible
                    if let Some(off) = sel_off {
                        self.start = i.saturating_sub(off);
                    }
                } else if s >= n {
                    self.sel = Some(n - 1);
                }
            }
            if self.start + self.page > n {
                self.start = n.saturating_sub(self.page);
            }
            self.ensure_visible();
        }
    }

    // ------------------------------------------------------------- layout

    fn layout(&self) -> Layout {
        let (w, h) = (self.scr.w as i32, self.scr.h as i32);
        let ch = round_pct(h, 32);
        let mh = round_pct(h, 40);
        let nh = h - ch - mh;
        let lw = round_pct(w, 45);
        let pw = w - lw;
        let cpu = Rect { x: 0, y: 0, w, h: ch };
        let mem = Rect { x: 0, y: ch, w: lw, h: mh };
        let net = Rect { x: 0, y: ch + mh, w: lw, h: nh };
        let mut procs = Rect { x: lw, y: ch, w: pw, h: mh + nh };
        let mut detail = None;
        if self.procs.detail.is_some() {
            detail = Some(Rect { x: procs.x, y: procs.y, w: procs.w, h: 8 });
            procs.y += 8;
            procs.h -= 8;
        }
        // cpu inner box
        let threads = self.cpu.threads as i32;
        let core_temp = self.cfg.check_temp && self.cpu.has_core_temps();
        let gpu_rows = if self.cfg.show_gpu { self.gpu.stats.len() as i32 } else { 0 };
        let avail = (ch - 2 - 2 - 2 - gpu_rows).max(1);
        let colw_of = |gw: i32| 4 + gw + 5 + if core_temp { 5 } else { 0 };
        let maxw = (w * 6 / 10).max(30);
        let needed = ((threads + avail - 1) / avail).max(1);
        let (mut cols, mut graphw) = (1, 10);
        for gw in [10, 8, 6, 5, 4, 0] {
            let max_cols = ((maxw - 1) / (colw_of(gw) + 1)).max(1);
            cols = needed.min(max_cols);
            graphw = gw;
            if needed <= max_cols {
                break;
            }
        }
        let colw = colw_of(graphw);
        let core_rows = ((threads + cols - 1) / cols).min(avail);
        let cin_w = (cols * colw + (cols - 1) + 2).max(30);
        let cin_h = (core_rows + 2 + gpu_rows + 2).min(ch - 2);
        let cin = Rect { x: w - 2 - cin_w, y: 1 + ((ch - 2 - cin_h) / 2), w: cin_w, h: cin_h };
        Layout { cpu, mem, net, procs, detail, cin, cols, colw, graphw, core_rows, core_temp }
    }

    // ------------------------------------------------------------- drawing

    fn btn(&mut self, x: i32, y: i32, line: Color, parts: &[(&str, Color, bool)], key: Key) -> i32 {
        let w = tag_len(parts);
        self.buttons.push(Button { x, w, y, key });
        tag(&mut self.scr, x, y, line, parts)
    }

    fn draw(&mut self) {
        let t = Instant::now();
        self.buttons.clear();
        self.scr.clear(self.th.main_fg, self.th.main_bg);
        let (w, h) = (self.scr.w as i32, self.scr.h as i32);
        if w < 80 || h < 24 {
            let th = &self.th;
            let msg = ["Terminal size too small:", &format!("Width = {w} Height = {h}"), "", "Needed for current config:", "Width = 80 Height = 24"];
            for (i, m) in msg.iter().enumerate() {
                let x = (w - m.chars().count() as i32) / 2;
                self.scr.text(x.max(0), h / 2 - 2 + i as i32, m, if i == 1 { 0xff4040 } else { th.title }, true);
            }
            self.scr.flush();
            return;
        }
        let lay = self.layout();
        self.page = (lay.procs.h - 3).max(1) as usize;
        self.rebuild_rows();
        self.draw_cpu(&lay);
        self.draw_mem(&lay);
        self.draw_net(&lay);
        if let Some(d) = lay.detail {
            self.draw_detail(d);
        }
        self.draw_procs(&lay);
        match self.mode.clone() {
            Mode::Normal => {}
            Mode::Menu { sel } => self.draw_menu(sel),
            Mode::Options { sel, edit } => self.draw_options(sel, edit),
            Mode::Help { page } => self.draw_help(page),
            Mode::Signal { verb, signame, pid, name, sel, result, .. } => self.draw_signal(verb, signame, pid, &name, sel, result),
        }
        self.draw_us = t.elapsed().as_micros();
        self.scr.flush();
    }

    fn draw_cpu(&mut self, lay: &Layout) {
        let r = lay.cpu;
        let th = &self.th;
        let s = &mut self.scr;
        draw_box(s, r.x, r.y, r.w, r.h, th.cpu_box);
        tag(s, r.x + 2, r.y, th.cpu_box, &[("cpu", th.title, true)]);
        let (title, hi, cbox) = (th.title, th.hi_fg, th.cpu_box);
        self.btn(r.x + 10, r.y, cbox, &[("m", hi, true), ("enu", title, true)], Key::Char('m'));
        // update ms (right)
        let ums = format!(" {}ms ", self.cfg.update_ms);
        let ux = r.x + r.w - (ums.len() as i32) - 6;
        let s = &mut self.scr;
        s.set(ux, r.y, '┤', cbox, false);
        self.buttons.push(Button { x: ux + 1, w: 1, y: r.y, key: Key::Char('+') });
        self.buttons.push(Button { x: ux + 2 + ums.len() as i32, w: 1, y: r.y, key: Key::Char('-') });
        let mut x = s.text(ux + 1, r.y, "+", hi, true);
        x = s.text(x, r.y, &ums, title, true);
        x = s.text(x, r.y, "-", hi, true);
        s.set(x, r.y, '├', cbox, false);
        // clock (center)
        let clock = strftime(&self.cfg.draw_clock);
        if !clock.is_empty() {
            let cx = r.x + (r.w - clock.chars().count() as i32 - 2) / 2;
            tag(s, cx, r.y, cbox, &[(&clock, title, true)]);
        }
        // battery (issue #199), left of update string
        if self.cfg.show_battery && self.bat.present() {
            if let Some(p) = self.bat.percent {
                let sym = match self.bat.status.as_str() {
                    "Charging" => "▲",
                    "Discharging" => "▼",
                    "Full" => "■",
                    _ => "○",
                };
                let txt = format!("BAT{sym} {p}% ");
                let bw = 10;
                let bx = ux - (txt.chars().count() as i32 + bw + 3);
                let th = &self.th;
                s.set(bx, r.y, '┤', cbox, false);
                let x = s.text(bx + 1, r.y, &txt, title, true);
                // battery: red when low
                let mut g = th.cpu;
                g.reverse();
                meter(s, x, r.y, bw, p as f32, &g, th.inactive_fg);
                s.set(x + bw, r.y, '├', cbox, false);
            }
        }
        // uptime on bottom border
        let th = &self.th;
        let up = format!("up {}", fmt_duration(self.cpu.uptime));
        tag(s, r.x + r.w - up.len() as i32 - 4, r.y + r.h - 1, cbox, &[(&up, th.title, false)]);
        // -d: timings on the cpu box bottom border (free space, covers nothing)
        if self.debug {
            let dbg = format!("collect {}µs procs {}µs draw {}µs", self.collect_us, self.procs.collect_us, self.draw_us);
            tag(s, r.x + 2, r.y + r.h - 1, cbox, &[(&dbg, 0xffff00, false)]);
        }

        // graph area (upper normal, lower mirrored like bashtop)
        let c = lay.cin;
        let gx = r.x + 1;
        let gw = c.x - gx - 1;
        let gh = r.h - 2;
        let ha = (gh + 1) / 2;
        let hb = gh - ha;
        let hires = self.cfg.hires_graphs;
        graph_dq(s, gx, r.y + 1, gw, ha, &self.cpu.hist[0], &th.cpu, false, hires);
        graph_dq(s, gx, r.y + 1 + ha, gw, hb, &self.cpu.hist[0], &th.cpu, true, hires);

        // inner box
        draw_box(s, c.x, c.y, c.w, c.h, th.div_line);
        let model_max = (c.w - 14).max(6) as usize;
        tag(s, c.x + 2, c.y, th.div_line, &[(&trunc(&self.cpu.model, model_max), th.title, true)]);
        if let Some(f) = self.cpu.freq {
            let fs = if f >= 1.0 { format!("{f:.1} GHz") } else { format!("{:.0} MHz", f * 1000.0) };
            tag(s, c.x + c.w - fs.len() as i32 - 4, c.y, th.div_line, &[(&fs, th.title, false)]);
        }
        let ix = c.x + 1;
        let iw = c.w - 2;
        let mut y = c.y + 1;
        // total line
        let pct = self.cpu.usage[0];
        let temp_s = if self.cfg.check_temp { self.cpu.temp.map(|t| format!(" {:>3.0}°C", t)) } else { None };
        let tw = temp_s.as_ref().map_or(0, |t| t.chars().count() as i32);
        s.text(ix, y, "CPU", th.main_fg, true);
        let mw = iw - 4 - 5 - tw;
        meter(s, ix + 4, y, mw, pct, &th.cpu, th.inactive_fg);
        s.text(ix + 4 + mw, y, &rpad(&format!("{:.0}%", pct), 5), th.main_fg, true);
        if let (Some(ts), Some(t)) = (&temp_s, self.cpu.temp) {
            let tc = th.temp[((t / self.cpu.temp_crit.max(1.0)) * 100.0).clamp(0.0, 100.0) as usize];
            s.text(ix + iw - tw, y, ts, tc, false);
        }
        y += 1;
        // cores
        let threads = self.cpu.threads as i32;
        let shown = (lay.core_rows * lay.cols).min(threads);
        for i in 0..shown {
            let col = i / lay.core_rows;
            let row = i % lay.core_rows;
            let cx = ix + col * (lay.colw + 1);
            let cy = y + row;
            let u = self.cpu.usage[(i + 1) as usize];
            let name = if threads >= 100 { format!("{}", i) } else { format!("C{}", i) };
            s.text(cx, cy, &name, th.main_fg, false);
            let h = &self.cpu.hist[(i + 1) as usize];
            if lay.graphw > 0 {
                mini(s, cx + 4, cy, lay.graphw, h.iter().copied(), h.len(), &th.cpu, hires);
            }
            s.text(cx + 4 + lay.graphw, cy, &rpad(&format!("{:.0}%", u), 5), th.cpu[u.clamp(0.0, 100.0) as usize], false);
            if lay.core_temp {
                if let Some(t) = self.cpu.core_temps[i as usize] {
                    let tc = th.temp[((t / self.cpu.temp_crit.max(1.0)) * 100.0).clamp(0.0, 100.0) as usize];
                    s.text(cx + 4 + lay.graphw + 5, cy, &rpad(&format!("{:.0}°C", t), 5), tc, false);
                }
            }
        }
        y += lay.core_rows;
        // GPUs (issues #26, #221)
        if self.cfg.show_gpu {
            for (gi, g) in self.gpu.stats.iter().enumerate() {
                if y >= c.y + c.h - 2 {
                    break;
                }
                let label = if self.gpu.stats.len() > 1 { format!("GPU{gi}") } else { "GPU".to_string() };
                s.text(ix, y, &label, th.main_fg, true);
                let extra = if g.mem_total > 0 { format!(" {}/{}", fmt_short(g.mem_used as f64), fmt_short(g.mem_total as f64)) } else { String::new() };
                let tstr = g.temp.map(|t| format!(" {t:.0}°C")).unwrap_or_default();
                let tail = format!("{:>4.0}%{}{}", g.util, extra, tstr);
                let tl = tail.chars().count() as i32;
                let mw = (iw - label.len() as i32 - 1 - tl).max(1);
                meter(s, ix + label.len() as i32 + 1, y, mw, g.util, &th.cpu, th.inactive_fg);
                s.text(ix + iw - tl, y, &tail, th.main_fg, false);
                y += 1;
            }
        }
        // load average
        let la = format!("Load AVG: {:>5.2} {:>5.2} {:>5.2}", self.cpu.load[0], self.cpu.load[1], self.cpu.load[2]);
        s.text_max(ix, c.y + c.h - 2, &la, iw as usize, th.main_fg, false);
        if shown < threads {
            s.text(ix + iw - 6, c.y + c.h - 2, &format!("+{}", threads - shown), th.inactive_fg, false);
        }
    }

    fn draw_mem(&mut self, lay: &Layout) {
        let r = lay.mem;
        let th = &self.th;
        let s = &mut self.scr;
        draw_box(s, r.x, r.y, r.w, r.h, th.mem_box);
        tag(s, r.x + 2, r.y, th.mem_box, &[("mem", th.title, true)]);
        let inner_w = r.w - 2;
        let show_disks = inner_w >= 44 && !self.disks.list.is_empty();
        let mw = if show_disks { inner_w / 2 } else { inner_w };
        if show_disks {
            let dx = r.x + 1 + mw;
            s.set(dx, r.y, '┬', th.mem_box, false);
            s.set(dx, r.y + r.h - 1, '┴', th.mem_box, false);
            for y in r.y + 1..r.y + r.h - 1 {
                s.set(dx, y, '│', th.div_line, false);
            }
            tag(s, dx + 2, r.y, th.mem_box, &[("disks", th.title, true)]);
        }
        // ---- memory section
        let x = r.x + 1;
        let w = mw - 1;
        let rows = r.h - 2;
        let m = &self.mem;
        let mut items: Vec<(&str, u64, f32, &theme::Gradient, usize)> = vec![
            ("Used:", m.used, m.used as f32 * 100.0 / m.total as f32, &th.used, 0),
            ("Available:", m.available, m.available as f32 * 100.0 / m.total as f32, &th.available, 1),
            ("Cached:", m.cached, m.cached as f32 * 100.0 / m.total as f32, &th.cached, 2),
            ("Free:", m.free, m.free as f32 * 100.0 / m.total as f32, &th.free, 3),
        ];
        let show_swap = self.cfg.show_swap && m.swap_total > 0;
        let tall = rows >= 1 + 8 + if show_swap { 5 } else { 0 };
        let mut y = r.y + 1;
        let total = fmt_bytes(m.total as f64);
        s.text(x, y, "Total:", th.title, true);
        s.text(x + w - total.len() as i32, y, &total, th.title, true);
        y += 1;
        let swap_items: Vec<(&str, u64, f32, &theme::Gradient, usize)> = if show_swap {
            vec![
                ("Used:", m.swap_used, m.swap_used as f32 * 100.0 / m.swap_total as f32, &th.used, 4),
                ("Free:", m.swap_free, m.swap_free as f32 * 100.0 / m.swap_total as f32, &th.free, 5),
            ]
        } else {
            vec![]
        };
        let hires = self.cfg.hires_graphs;
        let draw_item = |s: &mut Screen, y: &mut i32, it: &(&str, u64, f32, &theme::Gradient, usize), tall: bool| {
            let (label, val, pct, grad, hi) = *it;
            let vs = fmt_bytes(val as f64);
            let ps = format!("{:>4.0}%", pct);
            if tall {
                s.text(x, *y, label, th.main_fg, false);
                s.text(x + w - vs.len() as i32, *y, &vs, th.main_fg, false);
                *y += 1;
                let gw = w - 6;
                if self.cfg.mem_graphs {
                    let h = &self.mem.hist[hi];
                    mini(s, x, *y, gw, h.iter().copied(), h.len(), grad, hires);
                } else {
                    meter(s, x, *y, gw, pct, grad, th.inactive_fg);
                }
                s.text(x + w - 5, *y, &ps, th.main_fg, false);
                *y += 1;
            } else {
                let lw = 6;
                s.text_max(x, *y, label.trim_end_matches(':'), lw as usize, th.main_fg, false);
                let tail = format!("{} {}", ps, fmt_short(val as f64));
                let tl = tail.chars().count() as i32;
                let gw = (w - lw - 1 - tl).max(1);
                if self.cfg.mem_graphs {
                    let h = &self.mem.hist[hi];
                    mini(s, x + lw, *y, gw, h.iter().copied(), h.len(), grad, hires);
                } else {
                    meter(s, x + lw, *y, gw, pct, grad, th.inactive_fg);
                }
                s.text(x + w - tl, *y, &tail, th.main_fg, false);
                *y += 1;
            }
        };
        let bottom = r.y + r.h - 1;
        for it in items.drain(..) {
            if y + if tall { 2 } else { 1 } > bottom {
                break;
            }
            draw_item(s, &mut y, &it, tall);
        }
        if show_swap && y + 2 <= bottom {
            if tall {
                y += 0;
            }
            let st = fmt_bytes(m.swap_total as f64);
            s.text(x, y, "Swap:", th.title, true);
            s.text(x + w - st.len() as i32, y, &st, th.title, true);
            y += 1;
            for it in &swap_items {
                if y + if tall { 2 } else { 1 } > bottom {
                    break;
                }
                draw_item(s, &mut y, it, tall);
            }
        }
        // zram (issue #119)
        if self.cfg.show_swap {
            for z in &self.mem.zram {
                if y >= bottom {
                    break;
                }
                let ratio = if z.compr > 0 { z.orig as f64 / z.compr as f64 } else { 0.0 };
                let line = format!("{}: {} → {} ({:.1}x)", z.name, fmt_short(z.orig as f64), fmt_short(z.used as f64), ratio);
                s.text_max(x, y, &line, w as usize, th.inactive_fg.max(th.main_fg), false);
                y += 1;
            }
        }
        // ---- disks
        if !show_disks {
            return;
        }
        let x = r.x + 1 + mw + 1;
        let w = inner_w - mw - 1;
        let n = self.disks.list.len() as i32;
        let per = if rows >= n * 3 { 3 } else if rows >= n * 2 { 2 } else { 1 };
        let mut y = r.y + 1;
        for d in &self.disks.list {
            if y + per > bottom + 0 && y >= bottom {
                break;
            }
            if y >= bottom {
                break;
            }
            let size = fmt_short(d.total as f64);
            let io = if d.io && (d.read_bps > 0.0 || d.write_bps > 0.0 || per >= 2) {
                format!("r:{} w:{}", fmt_short(d.read_bps), fmt_short(d.write_bps))
            } else {
                String::new()
            };
            let namew = (w - size.len() as i32 - 1).max(4);
            s.text_max(x, y, &d.name, (namew as usize).min(20), th.title, true);
            if w >= 30 && !io.is_empty() && per >= 2 {
                let ix = x + w - size.len() as i32 - 1 - io.len() as i32;
                if ix > x + d.name.chars().count().min(20) as i32 {
                    s.text(ix, y, &io, th.inactive_fg.max(0x606060), false);
                }
            }
            if per == 1 {
                // name + used meter + pct in one line
                let ps = format!("{:>4.0}%", d.used_pct);
                let nm = 9;
                let gw = (w - nm - ps.len() as i32 - size.len() as i32 - 1).max(1);
                s.text_max(x, y, &d.name, nm as usize - 1, th.title, true);
                meter(s, x + nm, y, gw, d.used_pct, &th.used, th.inactive_fg);
                s.text(x + nm + gw, y, &ps, th.main_fg, false);
                s.text(x + w - size.len() as i32, y, &size, th.main_fg, false);
                y += 1;
                continue;
            }
            s.text(x + w - size.len() as i32, y, &size, th.main_fg, false);
            y += 1;
            let lines: [(&str, f32, u64, &theme::Gradient); 2] = [("Used:", d.used_pct, d.used, &th.used), ("Free:", 100.0 - d.used_pct, d.free, &th.free)];
            for (i, (label, pct, val, grad)) in lines.iter().enumerate() {
                if i + 1 >= per as usize + 0 && i >= (per - 1) as usize {
                    break;
                }
                if y >= bottom {
                    break;
                }
                let ps = format!("{:>4.0}%", pct);
                let vs = fmt_short(*val as f64);
                s.text(x, y, label, th.main_fg, false);
                let gw = (w - 6 - ps.len() as i32 - vs.len() as i32 - 2).max(1);
                s.text(x + 6, y, &ps, th.main_fg, false);
                meter(s, x + 6 + ps.len() as i32 + 1, y, gw, *pct, grad, th.inactive_fg);
                s.text(x + w - vs.len() as i32, y, &vs, th.main_fg, false);
                y += 1;
            }
        }
    }

    fn draw_net(&mut self, lay: &Layout) {
        let r = lay.net;
        let (cbox, title, hi, div, mainfg) = (self.th.net_box, self.th.title, self.th.hi_fg, self.th.div_line, self.th.main_fg);
        draw_box(&mut self.scr, r.x, r.y, r.w, r.h, cbox);
        tag(&mut self.scr, r.x + 2, r.y, cbox, &[("net", title, true)]);
        let dev = format!(" {} ", trunc(self.net.current(), 12));
        let parts_len = tag_len(&[("b", hi, true), ("<", title, false), (&dev, title, true), (">", title, false), ("n", hi, true)]);
        let bx = r.x + r.w - parts_len - 2;
        self.buttons.push(Button { x: bx, w: 3, y: r.y, key: Key::Char('b') });
        self.buttons.push(Button { x: bx + parts_len - 3, w: 3, y: r.y, key: Key::Char('n') });
        tag(&mut self.scr, bx, r.y, cbox, &[("b", hi, true), ("<", title, false), (&dev, title, true), (">", title, false), ("n", hi, true)]);
        self.btn(r.x + r.w - 12, r.y + r.h - 1, cbox, &[("z", hi, true), (" reset", title, false)], Key::Char('z'));
        if let Some((m, t)) = &self.status {
            if t.elapsed() < Duration::from_secs(3) {
                let m = m.clone();
                let pm = self.th.proc_misc;
                tag(&mut self.scr, r.x + r.w - 14 - m.chars().count() as i32 - 2, r.y + r.h - 1, cbox, &[(&m, pm, true)]);
            }
        }

        let th = &self.th;
        let s = &mut self.scr;
        let ih = r.h - 2;
        if ih < 2 {
            return;
        }
        let sw = 24.min(r.w / 2);
        let sx = r.x + 1;
        draw_box(s, sx, r.y + 1, sw, ih, div);
        tag(s, sx + 2, r.y + 1, div, &[("Download", title, true)]);
        tag(s, sx + 2, r.y + ih, div, &[("Upload", title, true)]);
        let n = &self.net;
        let iw = sw - 2;
        let half = (ih - 2) / 2;
        let dl = [
            format!("▼ {}/s", fmt_bytes(n.rx_bps)),
            format!("▼ ({})", fmt_bits(n.rx_bps)),
            format!("Top: {}/s", fmt_bytes(n.rx_top)),
            format!("Total: {}", fmt_bytes(n.rx_total as f64)),
        ];
        let ul = [
            format!("▲ {}/s", fmt_bytes(n.tx_bps)),
            format!("▲ ({})", fmt_bits(n.tx_bps)),
            format!("Top: {}/s", fmt_bytes(n.tx_top)),
            format!("Total: {}", fmt_bytes(n.tx_total as f64)),
        ];
        let lines_each = half.min(4).max(1) as usize;
        let order = [0usize, 3, 1, 2]; // most important first when space is short
        let mut pick: Vec<usize> = order.iter().copied().take(lines_each).collect();
        pick.sort();
        for (i, &k) in pick.iter().enumerate() {
            if 2 + i as i32 >= ih {
                break;
            }
            let c = if k == 0 { title } else { mainfg };
            s.text_max(sx + 1, r.y + 2 + i as i32, &dl[k], iw as usize, c, k == 0);
        }
        let base = r.y + ih - pick.len() as i32;
        for (i, &k) in pick.iter().enumerate() {
            let yy = base + i as i32;
            if yy <= r.y + 1 + pick.len() as i32 {
                continue;
            }
            let c = if k == 0 { title } else { mainfg };
            s.text_max(sx + 1, yy, &ul[k], iw as usize, c, k == 0);
        }
        // graphs (auto-scaling, issue-free bashtop behaviour)
        let gx = sx + sw + 1;
        let gw = r.x + r.w - 1 - gx;
        let ha = (ih + 1) / 2;
        let hb = ih - ha;
        let per = if self.cfg.hires_graphs { 2 } else { 1 };
        let need = (gw.max(0) as usize) * per;
        let scale = |h: &std::collections::VecDeque<f64>| -> f64 { h.iter().rev().take(need).fold(10240.0_f64, |a, &b| a.max(b)) };
        let (srx, stx) = (scale(&n.hist_rx), scale(&n.hist_tx));
        let rx: Vec<f32> = n.hist_rx.iter().map(|v| (v / srx * 100.0) as f32).collect();
        let tx: Vec<f32> = n.hist_tx.iter().map(|v| (v / stx * 100.0) as f32).collect();
        let hires = self.cfg.hires_graphs;
        graph(s, gx, r.y + 1, gw, ha, rx.iter().copied(), rx.len(), &th.download, false, hires);
        graph(s, gx, r.y + 1 + ha, gw, hb, tx.iter().copied(), tx.len(), &th.upload, true, hires);
        // scale labels
        s.text(gx, r.y + 1, &fmt_short(srx), th.inactive_fg.max(0x505050), false);
        s.text(gx, r.y + ih, &fmt_short(stx), th.inactive_fg.max(0x505050), false);
    }

    fn draw_detail(&mut self, r: Rect) {
        let th = &self.th;
        let s = &mut self.scr;
        let Some(d) = &self.procs.detail else { return };
        draw_box(s, r.x, r.y, r.w, r.h, th.proc_box);
        let p = self.procs.map.get(&d.pid);
        let title = format!("{} {}", d.pid, trunc(&d.name, 24));
        tag(s, r.x + 2, r.y, th.proc_box, &[(&title, th.title, true)]);
        let (hi, tl, pb) = (th.hi_fg, th.title, th.proc_box);
        let iw = r.w - 2;
        let gw = (iw / 3).clamp(10, 40);
        let gx = r.x + 1;
        let gh = r.h - 2;
        graph_dq(s, gx, r.y + 1, gw, gh, &d.hist, &th.cpu, false, self.cfg.hires_graphs);
        let cpu_now = d.hist.back().copied().unwrap_or(0.0);
        s.text(gx, r.y + 1, &format!("{:.1}%", cpu_now), th.title, true);
        let x = gx + gw + 2;
        let w = r.x + r.w - 1 - x;
        let mut y = r.y + 1;
        match p {
            Some(p) => {
                let status = match p.state {
                    'R' => "Running",
                    'S' => "Sleeping",
                    'D' => "Disk sleep",
                    'Z' => "Zombie",
                    'T' => "Stopped",
                    't' => "Traced",
                    'I' => "Idle",
                    'X' => "Dead",
                    _ => "Unknown",
                };
                let elapsed = self.cpu.uptime - p.start as f64 / self.procs.clk_tck();
                let parent = self.procs.map.get(&p.ppid).map(|pp| pp.name.clone()).unwrap_or_else(|| p.ppid.to_string());
                let pairs: [(&str, String); 6] = [
                    ("Status:", status.to_string()),
                    ("Elapsed:", fmt_duration(elapsed)),
                    ("Parent:", parent),
                    ("User:", self.procs.user(p.uid)),
                    ("Threads:", p.threads.to_string()),
                    ("Nice:", p.nice.to_string()),
                ];
                let ncols = if w >= 66 { 3 } else { 2 };
                let cw = w / ncols;
                for (i, (k, v)) in pairs.iter().enumerate() {
                    let (cx, cy) = (x + (i as i32 % ncols) * cw, y + i as i32 / ncols);
                    let nx = s.text(cx, cy, k, th.title, true);
                    s.text_max(nx + 1, cy, v, (cw - k.len() as i32 - 2).max(0) as usize, th.main_fg, false);
                }
                y += (pairs.len() as i32 + ncols - 1) / ncols;
                let ms = fmt_bytes(p.rss as f64);
                let nx = s.text(x, y, "Memory:", th.title, true);
                let nx = s.text(nx + 1, y, &format!("{:>4.1}% ", p.mem_pct), th.main_fg, false);
                let mw = (w - (nx - x) - ms.len() as i32 - 1).clamp(1, 40);
                meter(s, nx, y, mw, p.mem_pct, &th.used, th.inactive_fg);
                s.text(nx + mw + 1, y, &ms, th.main_fg, false);
                y += 1;
                if y < r.y + r.h - 2 {
                    let io = match (d.io_read, d.io_write) {
                        (Some(r), Some(wr)) => format!("r {}/s w {}/s (total r {} w {})", fmt_short(d.io_rps), fmt_short(d.io_wps), fmt_short(r as f64), fmt_short(wr as f64)),
                        _ => "no permission".into(),
                    };
                    let nx = s.text(x, y, "IO:", th.title, true);
                    s.text_max(nx + 1, y, &io, (w - 4).max(0) as usize, th.main_fg, false);
                    y += 1;
                }
                let cmd = if p.cmd.is_empty() { format!("[{}]", p.name) } else { p.cmd.clone() };
                s.text_max(x, y, &cmd, w as usize, th.inactive_fg.max(0x707070), false);
                // container in the title bar (issue #97)
                if let Some(c) = &p.container {
                    let tx = r.x + 2 + tag_len(&[(&title, 0, false)]) + 1;
                    tag(s, tx, r.y, th.proc_box, &[(&trunc(c, (r.w / 2) as usize), th.proc_misc, true)]);
                }
            }
            None => {
                s.text(x, y, "Process has exited.", 0xff6060, true);
            }
        }
        // bottom: close hint
        let bx = r.x + r.w - 12;
        let _ = (hi, tl, pb);
        self.btn(bx, r.y + r.h - 1, pb, &[("↵", hi, true), (" close", tl, false)], Key::Enter);
    }

    fn draw_procs(&mut self, lay: &Layout) {
        let r = lay.procs;
        let (pb, title, hi, inactive, mainfg) = (self.th.proc_box, self.th.title, self.th.hi_fg, self.th.inactive_fg, self.th.main_fg);
        draw_box(&mut self.scr, r.x, r.y, r.w, r.h, pb);
        tag(&mut self.scr, r.x + 2, r.y, pb, &[("processes", title, true)]);
        // top-right controls, right to left; skipped when they don't fit
        let min_x = r.x + 2 + tag_len(&[("processes", title, true)]) + 1;
        let sort = self.cfg.proc_sorting.clone();
        let mut x = r.x + r.w - 2;
        let sort_parts: [(&str, Color, bool); 3] = [("‹ ", hi, true), (&sort, title, true), (" ›", hi, true)];
        let sl = tag_len(&sort_parts);
        if x - sl >= min_x {
            x -= sl;
            tag(&mut self.scr, x, r.y, pb, &sort_parts);
            self.buttons.push(Button { x: x + 1, w: 2, y: r.y, key: Key::Left });
            self.buttons.push(Button { x: x + sl - 3, w: 2, y: r.y, key: Key::Right });
        }
        // an active filter has priority over the tree/reverse buttons
        let reserve = if self.filter_input || !self.filter.is_empty() { (self.filter.chars().count() as i32 + 8).min(24) } else { 0 };
        let rev_c = if self.cfg.proc_reversed { title } else { inactive };
        let rparts: [(&str, Color, bool); 2] = [("r", hi, true), ("everse", rev_c, self.cfg.proc_reversed)];
        if x - tag_len(&rparts) - 1 - reserve >= min_x {
            x -= tag_len(&rparts) + 1;
            self.btn(x, r.y, pb, &rparts, Key::Char('r'));
        }
        let tree_c = if self.cfg.proc_tree { title } else { inactive };
        let tparts: [(&str, Color, bool); 2] = [("e", hi, true), (" tree", tree_c, self.cfg.proc_tree)];
        if x - tag_len(&tparts) - 1 - reserve >= min_x {
            x -= tag_len(&tparts) + 1;
            self.btn(x, r.y, pb, &tparts, Key::Char('e'));
        }
        // filter (always shown while typing, shortened from the left)
        let has_filter = !self.filter.is_empty();
        let clear_w = if has_filter && !self.filter_input { 4 } else { 0 };
        let avail = (x - 1 - clear_w - min_x - 3).max(0) as usize;
        let ftxt = if self.filter_input {
            format!(" {}_", self.filter)
        } else if !has_filter {
            "ilter".to_string()
        } else {
            format!(" {}", self.filter)
        };
        let ftxt: String = if ftxt.chars().count() > avail { ftxt.chars().skip(ftxt.chars().count() - avail).collect() } else { ftxt };
        if avail >= 4 {
            let fparts: [(&str, Color, bool); 2] = [("f", hi, true), (&ftxt, title, has_filter || self.filter_input)];
            let fl = tag_len(&fparts);
            let fx = x - 1 - clear_w - fl;
            self.btn(fx, r.y, pb, &fparts, Key::Char('f'));
            if clear_w > 0 {
                self.btn(fx + fl + 1, r.y, pb, &[("c", hi, true)], Key::Char('c'));
            }
        }

        // columns
        let iw = r.w - 2;
        let x0 = r.x + 1;
        let tree = self.cfg.proc_tree;
        let show_args = iw >= 60;
        let show_threads = iw >= 50;
        let show_user = iw >= 42;
        let show_graph = iw >= 90;
        let pid_w = 8;
        let thr_w = if show_threads { 9 } else { 0 };
        let user_w = if show_user { 10 } else { 0 };
        let mem_w = 6;
        let cpu_w = 6;
        let graph_w = if show_graph { 6 } else { 0 };
        let fixed = pid_w + thr_w + user_w + mem_w + cpu_w + graph_w;
        let rest = iw - fixed;
        let (prog_w, args_w) = if tree {
            if show_args { ((rest * 6 / 10).max(16), rest - (rest * 6 / 10).max(16)) } else { (rest, 0) }
        } else if show_args {
            (16.min(rest), (rest - 16).max(0))
        } else {
            (rest, 0)
        };
        let hy = r.y + 1;
        let sortk = self.cfg.proc_sorting.as_str();
        // Sorted column: accent colour, bold, with direction arrow (▲ ascending, ▼ descending)
        let numeric_desc = matches!(sortk, "threads" | "memory" | "cpu lazy" | "cpu responsive");
        let arrow = if numeric_desc != self.cfg.proc_reversed { "▼" } else { "▲" };
        let accent = self.th.proc_misc;
        let is_sorted = |k: &str| sortk == k || (k == "cpu" && sortk.starts_with("cpu"));
        let mut cx = x0;
        let mut hbtn: Vec<(i32, i32, u8)> = Vec::with_capacity(7);
        let mut header = |s: &mut Screen, txt: &str, w: i32, key: &str, right: bool, col: u8| {
            if w <= 0 {
                return;
            }
            let sorted = is_sorted(key);
            let label = if sorted { format!("{}{}", txt.trim_end_matches(':'), arrow) } else { txt.to_string() };
            let t = if right { rpad(&label, w as usize - 1) + " " } else { pad(&label, w as usize) };
            s.text(cx, hy, &t, if sorted { accent } else { title }, true);
            hbtn.push((cx, w, col));
            cx += w;
        };
        let s = &mut self.scr;
        header(s, "Pid:", pid_w, "pid", false, 0);
        header(s, if tree { "Tree:" } else { "Program:" }, prog_w, "program", false, 1);
        header(s, "Arguments:", args_w, "arguments", false, 2);
        header(s, "Threads:", thr_w, "threads", false, 3);
        header(s, "User:", user_w, "user", false, 4);
        header(s, "Mem%", mem_w, "memory", true, 5);
        header(s, "Cpu%", cpu_w, "cpu", true, 6);
        // column headers are clickable: sort by column, click again to reverse
        for (x, w, col) in hbtn {
            self.buttons.push(Button { x, w, y: hy, key: Key::SortCol(col) });
        }
        let s = &mut self.scr;

        // rows
        let rows = &self.procs.rows;
        let n = rows.len();
        let page = self.page;
        if self.start > n.saturating_sub(1) {
            self.start = n.saturating_sub(page.min(n));
        }
        let start = self.start;
        let th = &self.th;
        let lazy = sortk == "cpu lazy";
        for i in 0..page.min(n.saturating_sub(start)) {
            let idx = start + i;
            let row = &rows[idx];
            let Some(p) = self.procs.map.get(&row.pid) else { continue };
            let y = r.y + 2 + i as i32;
            let selected = self.sel == Some(idx);
            let fade = if self.cfg.proc_gradient { th.proc_fade[(i * 100 / page.max(1)).min(100)] } else { mainfg };
            let fg = if selected { th.selected_fg } else { fade };
            let stopped = p.state == 'T' || p.state == 't';
            if selected {
                for xx in x0..x0 + iw {
                    s.set_bg(xx, y, th.selected_bg);
                }
            }
            let mut cx = x0;
            s.text(cx, y, &pad(&p.pid.to_string(), pid_w as usize), fg, selected);
            cx += pid_w;
            let name = if tree { format!("{}{}", row.prefix, p.name) } else { p.name.clone() };
            let ncol = if stopped && !selected { inactive.max(0x707070) } else if selected { fg } else if tree { fg } else { fg };
            s.text(cx, y, &pad(&name, (prog_w - 1).max(0) as usize), ncol, selected || !self.cfg.proc_gradient && !tree);
            cx += prog_w;
            if args_w > 0 {
                let args = p.cmd.get(p.args_at..).unwrap_or("");
                s.text(cx, y, &pad(args, (args_w - 1).max(0) as usize), fg, selected);
                cx += args_w;
            }
            if thr_w > 0 {
                s.text(cx, y, &pad(&p.threads.to_string(), thr_w as usize), fg, selected);
                cx += thr_w;
            }
            if user_w > 0 {
                s.text(cx, y, &pad(&self.procs.user(p.uid), (user_w - 1) as usize), fg, selected);
                cx += user_w;
            }
            let mem_s = if p.mem_pct >= 10.0 { format!("{:.0}", p.mem_pct) } else { format!("{:.1}", p.mem_pct) };
            s.text(cx, y, &rpad(&mem_s, (mem_w - 1) as usize), fg, selected);
            cx += mem_w;
            let cv = if lazy { p.cpu_lazy } else { p.cpu };
            let cpu_s = if cv >= 100.0 { format!("{:.0}", cv) } else { format!("{:.1}", cv) };
            let cfg_c = if selected { fg } else if self.cfg.proc_gradient { th.cpu[(cv.clamp(0.0, 100.0)) as usize].max(0) } else { fg };
            s.text(cx, y, &rpad(&cpu_s, (cpu_w - 1) as usize), if cv > 0.5 { cfg_c } else { fg }, selected);
            cx += cpu_w;
            if graph_w > 0 {
                let m = p.mini;
                mini(s, cx, y, graph_w - 1, m.iter().map(|v| *v as f32), m.len(), &th.cpu, false);
            }
        }
        // bottom bar
        let by = r.y + r.h - 1;
        let active = self.sel.is_some() || self.procs.detail.is_some();
        let ac = if active { title } else { inactive };
        let mut bx = r.x + 1;
        bx = self.btn(bx, by, pb, &[("↑", hi, true), (" select ", title, false), ("↓", hi, true)], Key::Down);
        let items: [(&str, &str, char); 5] = [("↵", " info", '\r'), ("t", "erminate", 't'), ("k", "ill", 'k'), ("i", "nterrupt", 'i'), ("p", "ause", 'p')];
        for (k, rest, ch) in items {
            let need = tag_len(&[(k, hi, true), (rest, ac, false)]);
            if bx + need > r.x + r.w - 14 {
                break;
            }
            let key = if ch == '\r' { Key::Enter } else { Key::Char(ch) };
            bx = self.btn(bx, by, pb, &[(k, if active { hi } else { inactive }, true), (rest, ac, false)], key);
        }
        let count = format!(" {}/{} ", self.sel.map(|s| s + 1).unwrap_or(0), n);
        let s = &mut self.scr;
        tag(s, r.x + r.w - count.len() as i32 - 3, by, pb, &[(&count, title, false)]);
    }

    // ------------------------------------------------------------- overlays

    fn overlay_box(&mut self, w: i32, h: i32, color: Color, title: &str) -> Rect {
        self.scr.dim(self.th.inactive_fg);
        let (sw, sh) = (self.scr.w as i32, self.scr.h as i32);
        let w = w.min(sw - 2);
        let h = h.min(sh - 2);
        let r = Rect { x: (sw - w) / 2, y: (sh - h) / 2, w, h };
        let bg = if self.th.main_bg == term::DEFAULT { 0x101010 } else { self.th.main_bg };
        self.scr.fill(r.x, r.y, r.w, r.h, bg);
        draw_box(&mut self.scr, r.x, r.y, r.w, r.h, color);
        if !title.is_empty() {
            tag(&mut self.scr, r.x + 2, r.y, color, &[(title, self.th.title, true)]);
        }
        self.buttons.clear(); // only overlay buttons are clickable now
        r
    }

    fn draw_menu(&mut self, sel: usize) {
        const BANNER: [&str; 4] = ["█▀▀▄ █▀▀▄ ▀▀█▀▀ █▀▀█ █▀▀█", "█▄▄▀ █▀▀▄   █   █  █ █▄▄█", "█  █ █▄▄▀   █   █▄▄█ █   ", " bashtop · rust edition  "];
        let r = self.overlay_box(40, 16, self.th.cpu_box, "");
        let th = &self.th;
        for (i, l) in BANNER.iter().enumerate() {
            let c = th.cpu[(i * 33).min(100)];
            let x = r.x + (r.w - l.chars().count() as i32) / 2;
            self.scr.text(x, r.y + 1 + i as i32, l, if i == 3 { th.inactive_fg.max(0x808080) } else { c }, true);
        }
        let items = ["Options", "Help", "Quit"];
        let keys = [Key::Char('o'), Key::Char('h'), Key::Char('q')];
        for (i, it) in items.iter().enumerate() {
            let y = r.y + 6 + i as i32 * 3;
            let w = 20;
            let x = r.x + (r.w - w) / 2;
            let (fg, bg) = if i == sel { (self.th.selected_fg, self.th.selected_bg) } else { (self.th.title, term::DEFAULT) };
            if i == sel {
                self.scr.fill(x, y, w, 2, bg);
            }
            let tx = x + (w - it.len() as i32) / 2;
            self.scr.text(tx, y, &it[..1], self.th.hi_fg.max(if i == sel { fg } else { 0 }), true);
            self.scr.text(tx + 1, y, &it[1..], fg, true);
            for yy in y..y + 2 {
                self.buttons.push(Button { x, w, y: yy, key: keys[i] });
            }
        }
    }

    fn draw_help(&mut self, page: usize) {
        let sh = self.scr.h as i32;
        let h = (HELP.len() as i32 + 3).min(sh - 2);
        let r = self.overlay_box(76, h, self.th.cpu_box, "help");
        let per = (r.h - 3).max(1) as usize;
        let pages = HELP.len().div_ceil(per);
        let page = page.min(pages.saturating_sub(1));
        let th = &self.th;
        let s = &mut self.scr;
        s.text(r.x + 2, r.y + 1, &pad("Key:", 22), th.title, true);
        s.text(r.x + 24, r.y + 1, "Description:", th.title, true);
        for (i, (k, d)) in HELP.iter().skip(page * per).take(per).enumerate() {
            let y = r.y + 2 + i as i32;
            s.text(r.x + 2, y, k, th.main_fg, true);
            s.text_max(r.x + 24, y, d, (r.w - 26) as usize, th.main_fg, false);
        }
        if pages > 1 {
            tag(s, r.x + r.w - 20, r.y + r.h - 1, th.cpu_box, &[("pg", th.title, true), ("↑", th.hi_fg, true), (&format!(" {}/{} ", page + 1, pages), th.title, true), ("pg", th.title, true), ("↓", th.hi_fg, true)]);
        }
    }

    fn draw_options(&mut self, sel: usize, edit: Option<String>) {
        let sh = self.scr.h as i32;
        let r = self.overlay_box(96, (OPTIONS.len() as i32 + 4).min(sh - 2), self.th.cpu_box, "options");
        let th = &self.th;
        let s = &mut self.scr;
        let lw = 44;
        let visible = (r.h - 3).max(1) as usize;
        let start = sel.saturating_sub(visible - 1);
        for (i, o) in OPTIONS.iter().enumerate().skip(start).take(visible) {
            let y = r.y + 1 + (i - start) as i32;
            let val = if i == sel { edit.clone().map(|e| e + "_").unwrap_or_else(|| self.cfg.get(o.key)) } else { self.cfg.get(o.key) };
            let selected = i == sel;
            if selected {
                for xx in r.x + 1..r.x + lw {
                    s.set_bg(xx, y, th.selected_bg);
                }
            }
            let fg = if selected { th.selected_fg } else { th.title };
            s.text(r.x + 2, y, &pad(&o.key.replace('_', " "), 18), fg, true);
            let v = match o.kind {
                Kind::Str if val.is_empty() && edit.is_none() => "\"\"".to_string(),
                _ => val,
            };
            let shown = if selected && edit.is_none() { format!("‹ {} ›", trunc(&v, 18)) } else { trunc(&v, 22) };
            s.text(r.x + 21, y, &shown, if selected { th.selected_fg } else { th.main_fg }, selected);
        }
        // description
        for xx in r.y + 1..r.y + r.h - 1 {
            s.set(r.x + lw, xx, '│', th.div_line, false);
        }
        if let Some(o) = OPTIONS.get(sel) {
            for (i, d) in o.desc.iter().enumerate() {
                s.text_max(r.x + lw + 2, r.y + 2 + i as i32, d, (r.w - lw - 3) as usize, th.main_fg, false);
            }
            let hint = match o.kind {
                Kind::Bool | Kind::Choice(_) | Kind::Int => "←/→ change",
                Kind::Str => if edit.is_some() { "↵ save  Esc cancel" } else { "↵ edit" },
            };
            s.text(r.x + lw + 2, r.y + r.h - 2, hint, th.hi_fg, true);
        }
    }

    fn draw_signal(&mut self, verb: &str, signame: &str, pid: i32, name: &str, sel: usize, result: Option<Vec<String>>) {
        let r = self.overlay_box(44, 9, self.th.proc_box, verb);
        let th = &self.th;
        let center = |s: &mut Screen, y: i32, t: &str, c: Color, b: bool| {
            let x = r.x + (r.w - t.chars().count() as i32) / 2;
            s.text(x.max(r.x + 1), y, &trunc(t, (r.w - 2) as usize), c, b);
        };
        match result {
            None => {
                let mut v = verb.to_string();
                if let Some(f) = v.get_mut(0..1) {
                    f.make_ascii_uppercase();
                }
                center(&mut self.scr, r.y + 2, &format!("{v} {} ({pid})?", trunc(name, 20)), th.title, true);
                center(&mut self.scr, r.y + 3, &format!("Signal {signame}"), th.main_fg, false);
                let opts = ["Yes", "+Tree", "No"];
                let mut x = r.x + 4;
                for (i, o) in opts.iter().enumerate() {
                    let t = format!("[ {o} ]");
                    let (fg, bg) = if i == sel { (th.selected_fg, th.selected_bg) } else { (th.title, term::DEFAULT) };
                    let w = t.chars().count() as i32;
                    if i == sel {
                        self.scr.fill(x, r.y + 5, w, 1, bg);
                    }
                    self.scr.text(x, r.y + 5, &t, fg, true);
                    let key = match i {
                        0 => Key::Char('y'),
                        1 => Key::Char('a'),
                        _ => Key::Char('n'),
                    };
                    self.buttons.push(Button { x, w, y: r.y + 5, key });
                    x += w + 3;
                }
                center(&mut self.scr, r.y + 7, "+Tree also signals all children", th.inactive_fg.max(0x606060), false);
            }
            Some(lines) => {
                for (i, l) in lines.iter().take(3).enumerate() {
                    center(&mut self.scr, r.y + 2 + i as i32, l, if i == 0 { th.title } else { th.main_fg }, i == 0);
                }
                let w = 8;
                let x = r.x + (r.w - w) / 2;
                self.scr.fill(x, r.y + 6, w, 1, th.selected_bg);
                self.scr.text(x, r.y + 6, "[  Ok  ]", th.selected_fg, true);
                self.buttons.push(Button { x, w, y: r.y + 6, key: Key::Enter });
            }
        }
    }

    // ------------------------------------------------------------- input

    fn handle(&mut self, k: Key) {
        // mouse → map to keys
        match k {
            Key::MouseDown(x, y) => {
                let (x, y) = (x as i32, y as i32);
                if let Some(b) = self.buttons.iter().find(|b| y == b.y && x >= b.x && x < b.x + b.w) {
                    let key = b.key;
                    return self.handle(key);
                }
                if matches!(self.mode, Mode::Normal) {
                    let lay = self.layout();
                    let r = lay.procs;
                    if x > r.x && x < r.x + r.w - 1 && y >= r.y + 2 && y < r.y + r.h - 1 {
                        let idx = self.start + (y - r.y - 2) as usize;
                        if idx < self.procs.rows.len() {
                            if self.sel == Some(idx) {
                                return self.handle(Key::Enter);
                            }
                            self.sel = Some(idx);
                        }
                    }
                }
                return;
            }
            Key::WheelUp(_, _) | Key::WheelDown(_, _) => {
                let up = matches!(k, Key::WheelUp(..));
                match self.mode {
                    Mode::Normal => {
                        for _ in 0..3 {
                            self.handle(if up { Key::Up } else { Key::Down });
                        }
                    }
                    Mode::Options { .. } | Mode::Menu { .. } | Mode::Help { .. } => self.handle(if up { Key::Up } else { Key::Down }),
                    _ => {}
                }
                return;
            }
            _ => {}
        }
        if k == Key::CtrlC {
            self.quit = true;
            return;
        }
        if k == Key::CtrlZ {
            term::suspend(self.cfg.mouse);
            self.scr.invalidate();
            return;
        }
        match self.mode.clone() {
            Mode::Normal => self.handle_normal(k),
            Mode::Menu { sel } => match k {
                Key::Up | Key::BackTab => self.mode = Mode::Menu { sel: (sel + 2) % 3 },
                Key::Down | Key::Tab => self.mode = Mode::Menu { sel: (sel + 1) % 3 },
                Key::Enter => match sel {
                    0 => self.open_options(),
                    1 => self.mode = Mode::Help { page: 0 },
                    _ => self.quit = true,
                },
                Key::Char('o') | Key::Char('O') | Key::F(2) => self.open_options(),
                Key::Char('h') | Key::Char('H') | Key::F(1) => self.mode = Mode::Help { page: 0 },
                Key::Char('q') | Key::Char('Q') => self.quit = true,
                Key::Esc | Key::Char('m') | Key::Char('M') => self.mode = Mode::Normal,
                _ => {}
            },
            Mode::Help { page } => match k {
                Key::Down | Key::PgDn => self.mode = Mode::Help { page: page + 1 },
                Key::Up | Key::PgUp => self.mode = Mode::Help { page: page.saturating_sub(1) },
                _ => self.mode = Mode::Normal,
            },
            Mode::Options { sel, edit } => self.handle_options(k, sel, edit),
            Mode::Signal { sig, verb, signame, pid, name, sel, result } => {
                if result.is_some() {
                    if matches!(k, Key::Enter | Key::Esc | Key::Char(_)) {
                        self.mode = Mode::Normal;
                    }
                    return;
                }
                let act = |me: &mut App, tree: bool| {
                    let mut targets = vec![pid];
                    if tree {
                        targets.extend(me.procs.descendants(pid));
                    }
                    targets.retain(|&t| procs::signal_allowed(t));
                    let mut ok = 0;
                    let mut err = None;
                    for t in &targets {
                        match procs::send_signal(*t, sig) {
                            Ok(()) => ok += 1,
                            Err(e) => err = Some(e),
                        }
                    }
                    let mut lines = vec![format!("Sending signal {signame} to pid {pid}{}!", if tree { " + tree" } else { "" })];
                    if let Some(e) = err {
                        lines.push(format!("Failed: {e}"));
                        if ok > 0 {
                            lines.push(format!("{ok}/{} processes signalled", targets.len()));
                        }
                    } else {
                        lines.push(if targets.len() > 1 { format!("Success! ({} processes)", targets.len()) } else { "Success!".into() });
                    }
                    me.mode = Mode::Signal { sig, verb, signame, pid, name: name.clone(), sel, result: Some(lines) };
                };
                match k {
                    Key::Left | Key::BackTab => self.mode = Mode::Signal { sig, verb, signame, pid, name, sel: (sel + 2) % 3, result },
                    Key::Right | Key::Tab => self.mode = Mode::Signal { sig, verb, signame, pid, name, sel: (sel + 1) % 3, result },
                    Key::Char('y') | Key::Char('Y') => act(self, false),
                    Key::Char('a') | Key::Char('A') => act(self, true),
                    Key::Enter => match sel {
                        0 => act(self, false),
                        1 => act(self, true),
                        _ => self.mode = Mode::Normal,
                    },
                    Key::Char('n') | Key::Char('N') | Key::Esc | Key::Char('q') => self.mode = Mode::Normal,
                    _ => {}
                }
            }
        }
    }

    fn open_options(&mut self) {
        self.mode = Mode::Options { sel: 0, edit: None };
    }

    fn apply_config(&mut self, key: &str) {
        match key {
            "color_theme" => self.th = Theme::load(&self.cfg.color_theme),
            "disks_filter" => self.disks.set_filter(&self.cfg.disks_filter),
            "custom_cpu_name" => {
                if !self.cfg.custom_cpu_name.is_empty() {
                    self.cpu.model = self.cfg.custom_cpu_name.clone()
                }
            }
            "proc_sorting" | "proc_reversed" | "proc_tree" => self.rows_dirty = true,
            "show_gpu" => {
                self.gpu.shutdown();
                self.gpu = Gpus::new(self.cfg.show_gpu, self.cfg.update_ms);
            }
            _ => {}
        }
    }

    fn handle_options(&mut self, k: Key, sel: usize, edit: Option<String>) {
        let o = &OPTIONS[sel];
        if let Some(mut e) = edit {
            match k {
                Key::Enter => {
                    self.cfg.set(o.key, &e);
                    self.apply_config(o.key);
                    self.mode = Mode::Options { sel, edit: None };
                }
                Key::Esc => self.mode = Mode::Options { sel, edit: None },
                Key::Backspace => {
                    e.pop();
                    self.mode = Mode::Options { sel, edit: Some(e) };
                }
                Key::Char(c) => {
                    e.push(c);
                    self.mode = Mode::Options { sel, edit: Some(e) };
                }
                _ => {}
            }
            return;
        }
        let n = OPTIONS.len();
        match k {
            Key::Up | Key::BackTab => self.mode = Mode::Options { sel: (sel + n - 1) % n, edit: None },
            Key::Down | Key::Tab => self.mode = Mode::Options { sel: (sel + 1) % n, edit: None },
            Key::PgUp => self.mode = Mode::Options { sel: sel.saturating_sub(10), edit: None },
            Key::PgDn => self.mode = Mode::Options { sel: (sel + 10).min(n - 1), edit: None },
            Key::Left | Key::Right => {
                let dir: i64 = if k == Key::Right { 1 } else { -1 };
                match o.kind {
                    Kind::Bool => {
                        let v = self.cfg.get(o.key) == "true";
                        self.cfg.set(o.key, if v { "false" } else { "true" });
                    }
                    Kind::Int => {
                        let v: i64 = self.cfg.get(o.key).parse().unwrap_or(0);
                        self.cfg.set(o.key, &(v + dir * 100).max(100).to_string());
                    }
                    Kind::Choice(f) => {
                        let list = f();
                        let cur = self.cfg.get(o.key);
                        let i = list.iter().position(|x| *x == cur).unwrap_or(0) as i64;
                        let ni = (i + dir).rem_euclid(list.len() as i64) as usize;
                        self.cfg.set(o.key, &list[ni]);
                    }
                    Kind::Str => {}
                }
                self.apply_config(o.key);
            }
            Key::Enter => {
                if let Kind::Str = o.kind {
                    self.mode = Mode::Options { sel, edit: Some(self.cfg.get(o.key)) };
                }
            }
            Key::Esc | Key::Char('o') | Key::Char('O') | Key::F(2) | Key::Char('q') | Key::Char('m') | Key::Char('M') => {
                self.cfg.save();
                self.mode = Mode::Normal;
            }
            _ => {}
        }
    }

    fn sel_pid(&self) -> Option<i32> {
        if let Some(s) = self.sel {
            return self.procs.rows.get(s).map(|r| r.pid);
        }
        self.procs.detail.as_ref().filter(|d| d.alive).map(|d| d.pid)
    }

    fn signal_dialog(&mut self, c: char) {
        let Some(pid) = self.sel_pid() else { return };
        let name = self.procs.map.get(&pid).map(|p| p.name.clone()).unwrap_or_default();
        let (sig, verb, signame) = match c {
            't' => (libc::SIGTERM, "terminate", "SIGTERM - 15"),
            'k' => (libc::SIGKILL, "kill", "SIGKILL - 9"),
            'i' => (libc::SIGINT, "interrupt", "SIGINT - 2"),
            _ => {
                let stopped = matches!(procs::read_state(pid), Some('T') | Some('t'));
                if stopped { (libc::SIGCONT, "continue", "SIGCONT - 18") } else { (libc::SIGSTOP, "stop", "SIGSTOP - 19") }
            }
        };
        let result = (!procs::signal_allowed(pid)).then(|| vec![format!("Not sending {signame} to pid {pid}!"), "pid 0 and 1 (kernel, init/launchd) are".into(), "protected: that would stop the system".into()]);
        self.mode = Mode::Signal { sig, verb, signame, pid, name, sel: 0, result };
    }

    fn ensure_visible(&mut self) {
        if let Some(s) = self.sel {
            if s < self.start {
                self.start = s;
            } else if s >= self.start + self.page {
                self.start = s + 1 - self.page;
            }
        }
    }

    fn handle_normal(&mut self, k: Key) {
        self.rebuild_rows();
        let n = self.procs.rows.len();
        if self.filter_input {
            match k {
                Key::Char(c) => {
                    self.filter.push(c);
                    self.rows_dirty = true;
                    self.start = 0;
                    if self.sel.is_some() {
                        self.sel = Some(0);
                    }
                    return;
                }
                Key::Backspace => {
                    self.filter.pop();
                    self.rows_dirty = true;
                    return;
                }
                Key::Enter => {
                    self.filter_input = false;
                    return;
                }
                Key::Esc => {
                    self.filter_input = false;
                    self.filter.clear();
                    self.rows_dirty = true;
                    return;
                }
                _ => {} // navigation keys fall through
            }
        }
        let lower = |k: Key| match k {
            Key::Char(c) => Key::Char(c.to_ascii_lowercase()),
            o => o,
        };
        match lower(k) {
            Key::Esc | Key::Char('m') => self.mode = Mode::Menu { sel: 0 },
            Key::F(2) | Key::Char('o') => self.open_options(),
            Key::F(1) | Key::Char('h') => self.mode = Mode::Help { page: 0 },
            Key::Char('q') => self.quit = true,
            Key::Char('+') | Key::Char('a') => {
                if self.cfg.update_ms < 86_399_900 {
                    self.cfg.update_ms += 100;
                    self.next_update += Duration::from_millis(100);
                }
            }
            Key::Char('-') | Key::Char('s') => {
                if self.cfg.update_ms > 100 {
                    self.cfg.update_ms -= 100;
                    self.next_update -= Duration::from_millis(100);
                }
            }
            Key::Up => {
                self.sel = match self.sel {
                    None => None,
                    Some(0) => None,
                    Some(i) => Some(i - 1),
                };
                self.ensure_visible();
            }
            Key::Down | Key::Tab => {
                if n == 0 {
                    return;
                }
                self.sel = match self.sel {
                    None => Some(self.start.min(n - 1)),
                    Some(i) if i + 1 < n => Some(i + 1),
                    s => s,
                };
                self.ensure_visible();
            }
            Key::PgUp => {
                if self.start > 0 {
                    let d = self.page.min(self.start);
                    self.start -= d;
                    self.sel = self.sel.map(|s| s.saturating_sub(d));
                } else if self.sel.is_some() {
                    self.sel = None;
                }
            }
            Key::PgDn => {
                let max_start = n.saturating_sub(self.page);
                if self.start < max_start {
                    let d = self.page.min(max_start - self.start);
                    self.start += d;
                    self.sel = self.sel.map(|s| (s + d).min(n.saturating_sub(1)));
                } else if self.sel.is_some() {
                    self.sel = Some(n.saturating_sub(1));
                }
            }
            Key::Home => {
                self.start = 0;
                if self.sel.is_some() {
                    self.sel = Some(0);
                }
            }
            Key::End => {
                self.start = n.saturating_sub(self.page);
                if self.sel.is_some() {
                    self.sel = Some(n.saturating_sub(1));
                }
            }
            Key::Enter => {
                let sp = self.sel.and_then(|s| self.procs.rows.get(s)).map(|r| r.pid);
                let dp = self.procs.detail.as_ref().map(|d| d.pid);
                match (sp, dp) {
                    (Some(p), d) if Some(p) != d => {
                        self.procs.set_detail(Some(p));
                        self.sel = None;
                        self.collect();
                    }
                    (_, Some(_)) => self.procs.set_detail(None),
                    _ => {}
                }
            }
            Key::Left | Key::Right => {
                let i = SORTINGS.iter().position(|s| *s == self.cfg.proc_sorting).unwrap_or(6) as i32;
                let d = if lower(k) == Key::Right { 1 } else { -1 };
                self.cfg.proc_sorting = SORTINGS[(i + d).rem_euclid(SORTINGS.len() as i32) as usize].to_string();
                self.rows_dirty = true;
            }
            Key::SortCol(col) => {
                let cur = self.cfg.proc_sorting.clone();
                let want = match col {
                    6 if cur.starts_with("cpu") => cur.clone(),
                    6 => "cpu lazy".to_string(),
                    c => SORTINGS[(c as usize).min(5)].to_string(),
                };
                if want == cur {
                    self.cfg.proc_reversed = !self.cfg.proc_reversed;
                } else {
                    self.cfg.proc_sorting = want;
                    self.cfg.proc_reversed = false;
                }
                self.rows_dirty = true;
            }
            Key::Char('b') => self.net.switch(-1),
            Key::Char('n') => self.net.switch(1),
            Key::Char('e') => {
                self.cfg.proc_tree = !self.cfg.proc_tree;
                self.rows_dirty = true;
            }
            Key::Char('r') => {
                self.cfg.proc_reversed = !self.cfg.proc_reversed;
                self.rows_dirty = true;
            }
            Key::Char('f') => {
                self.filter_input = true;
                self.start = 0;
                if self.sel.is_some() {
                    self.sel = Some(0);
                }
            }
            Key::Char('c') => {
                if !self.filter.is_empty() {
                    self.filter.clear();
                    self.filter_input = false;
                    self.rows_dirty = true;
                }
            }
            Key::Char(c @ ('t' | 'k' | 'i' | 'p')) => self.signal_dialog(c),
            Key::Char('z') => {
                self.net.reset_totals();
                self.status = Some(("totals reset".into(), Instant::now()));
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------- loop

    fn run(&mut self) {
        let mut keys = Vec::with_capacity(32);
        self.next_update = Instant::now() + Duration::from_millis(self.cfg.update_ms);
        let mut dirty = true;
        let mut last_clock = String::new();
        loop {
            if term::TERMINATE.load(Ordering::Relaxed) {
                break;
            }
            if term::RESIZED.swap(false, Ordering::Relaxed) {
                if term::CONTINUED.swap(false, Ordering::Relaxed) {
                    term::enter(self.cfg.mouse);
                }
                let (w, h) = term::size();
                self.scr.resize(w, h);
                dirty = true;
            }
            let now = Instant::now();
            if now >= self.next_update {
                let paused = !self.cfg.background_update && !matches!(self.mode, Mode::Normal);
                if !paused {
                    self.collect();
                }
                self.next_update += Duration::from_millis(self.cfg.update_ms);
                if self.next_update < now {
                    self.next_update = now + Duration::from_millis(self.cfg.update_ms);
                }
                dirty = true;
            }
            let clock = strftime(&self.cfg.draw_clock);
            if clock != last_clock {
                last_clock = clock;
                dirty = true;
            }
            if dirty {
                self.draw();
                dirty = false;
            }
            if self.quit {
                break;
            }
            // sleep until next update, next clock second, or input
            let to_update = self.next_update.saturating_duration_since(Instant::now()).as_millis() as i32;
            let to_second = if self.cfg.draw_clock.is_empty() {
                i32::MAX
            } else {
                let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_millis()).unwrap_or(0);
                (1000 - ms as i32) + 2
            };
            if term::poll_input(to_update.min(to_second).max(1)) {
                keys.clear();
                term::read_keys(&mut keys);
                for k in keys.drain(..) {
                    self.handle(k);
                    if self.quit {
                        break;
                    }
                }
                dirty = true;
            }
            if self.quit {
                break;
            }
        }
    }
}

fn main() {
    let mut debug = false;
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "-v" | "--version" => {
                println!("rbtop {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "-d" | "--debug" => debug = true,
            "-h" | "--help" => {
                println!("rbtop {} — bashtop rewritten in Rust\n\nUSAGE: rbtop [-d|--debug] [-v|--version]\n\n  -d  show collect/draw timings in the process box\n\nConfig: ~/.config/rbtop/rbtop.cfg (bashtop.cfg is read as fallback)\nThemes: bashtop/bpytop/btop .theme files in ~/.config/rbtop/themes", env!("CARGO_PKG_VERSION"));
                return;
            }
            _ => {
                eprintln!("unknown argument: {a} (try --help)");
                std::process::exit(1);
            }
        }
    }
    if unsafe { libc::isatty(0) } == 0 || unsafe { libc::isatty(1) } == 0 {
        eprintln!("rbtop needs an interactive terminal");
        std::process::exit(1);
    }
    std::panic::set_hook(Box::new(|info| {
        term::leave();
        eprintln!("rbtop crashed: {info}");
    }));
    term::install_signals();
    let mut app = App::new(debug);
    term::enter(app.cfg.mouse);
    app.run();
    term::leave();
    app.gpu.shutdown();
    app.cfg.save();
}
