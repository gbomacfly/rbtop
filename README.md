# rbtop – bashtop in Rust

Ein Nachbau von [bashtop](https://github.com/aristocratos/bashtop) (aristocratos) in Rust:
gleiches Layout, **gleiche Tastenbelegung**, gleiches Config- und Theme-Format, aber ein
Bruchteil der CPU-Last. Dazu kommen die umsetzbaren Feature-Requests aus den GitHub-Issues
des Originals.

![rbtop](docs/screenshot.png)

Abhängigkeiten: nur das `libc`-Crate. Kein TUI-Framework, kein psutil, keine
externen Programme pro Tick.

## Speed

Gemessen in einer 2-Kern-VM, 160×45-Terminal, gleiche Bedingungen, CPU-Zeit inkl. Kindprozesse:

| Szenario | bashtop 0.9.25 | rbtop |
|---|---|---|
| ~70 Prozesse, 2500 ms Takt | 6,07 % eines Kerns | **0,05 %** |
| 1068 Prozesse, 2500 ms Takt | – | 0,37 % |
| 1068 Prozesse, 100 ms Takt | – | 7,8 % (≈ Kernel-Untergrenze für 1068× `/proc/<pid>/stat`) |
| Terminal-Ausgabe (70 Proz., 2500 ms) | 3,7 KiB/s | 0,5–0,7 KiB/s |

Warum es so schnell ist:

- **Diff-Renderer:** zwei Zellpuffer; geschrieben werden nur geänderte Zellen, mit
  minimalen Cursor-Sprüngen und SGR-Wechseln, alles in *einem* `write()` mit Synchronized
  Output (`?2026`).
- **Prozess-Hot-Path:** pro Tick wird pro Prozess nur `/proc/<pid>/stat` gelesen, via
  `openat()` relativ zu einem offenen `/proc`-Handle in einen wiederverwendeten Puffer.
  `cmdline`, UID und cgroup werden nur einmal pro neuer PID gelesen (bzw. nach `exec`).
- **Kein Polling:** `poll()` schläft bis zum nächsten Tick, zur nächsten Sekunde (Uhr)
  oder bis zum nächsten Tastendruck; Eingaben werden sofort gezeichnet.
- **nvidia-smi** läuft, wenn vorhanden, *einmal* dauerhaft im Streaming-Modus (`-lms`),
  statt jeden Tick neu gestartet zu werden.
- Release-Profil: LTO, `codegen-units=1`, `panic=abort`, gestrippt (~770 KB).

Mit `rbtop -d` siehst du die Zeiten für Sammeln/Zeichnen live in der Prozessbox.

## Tasten (wie im Original)

| Taste | Funktion |
|---|---|
| Esc, M, m | Hauptmenü |
| F2, O, o | Optionen |
| F1, H, h | Hilfe |
| Ctrl-C, Q, q | Beenden |
| +, A, a / -, S, s | Update-Timer ±100 ms |
| Up / Down (Tab) | Auswahl in der Prozessliste |
| Enter | Detailansicht des ausgewählten Prozesses (nochmal: schließen) |
| Pg Up / Pg Down | Eine Seite springen |
| Home / End | Erste / letzte Seite |
| Left / Right | Vorherige / nächste Sortierung |
| b, B / n, N | Vorheriges / nächstes Netzwerkgerät |
| E, e | Baumansicht umschalten |
| R, r | Sortierung umkehren |
| F, f | Filter eingeben (Enter übernimmt, Esc verwirft) |
| C, c | Filter löschen |
| T, t | SIGTERM an ausgewählten Prozess |
| K, k | SIGKILL an ausgewählten Prozess |
| I, i | SIGINT an ausgewählten Prozess |

**Neu** (alle Tasten waren im Original frei):

| Taste | Funktion |
|---|---|
| P, p | Pause/Fortsetzen: SIGSTOP bzw. SIGCONT, je nach aktuellem Status (#185) |
| Z, z | Download/Upload-Summen zurücksetzen (#63) |
| Ctrl-Z | rbtop in den Hintergrund (`fg` holt es zurück) |
| Maus | Scrollen und Klicken in der Prozessliste (Klick auf ausgewählte Zeile = Details), alle `┤…├`-Buttons sind klickbar |

Im Signal-Dialog gibt es zusätzlich **[ +Tree ]**: schickt das Signal an den Prozess und
alle seine Nachfahren (Wunsch aus #185, z. B. Blender-Renderjobs pausieren).

## Feature-Requests aus den bashtop-Issues

| Issue | Wunsch | Status in rbtop |
|---|---|---|
| #17 | iotop-artige I/O-Werte | ✅ Lese-/Schreibraten pro Disk (`/proc/diskstats`, **ZFS-Datasets über kstat**), pro Prozess in der Detailansicht (`/proc/<pid>/io`) |
| #26, #221 | GPU-Auslastung | ✅ AMD über sysfs (`gpu_busy_percent`, VRAM, Temperatur), NVIDIA über *ein* dauerlaufendes `nvidia-smi` |
| #48 | zenpower/k10temp-Temperaturen | ✅ hwmon: coretemp (pro Kern), k10temp (Tdie/Tctl), zenpower, cpu_thermal (ARM), acpitz/x86_pkg_temp als Fallback |
| #63 | Netzwerk-Summen zurücksetzen | ✅ Taste `z` |
| #73 | Graphen statt Balken für Speicher | ✅ Option `mem_graphs` |
| #97 | Docker-Container-Infos | ✅ Container pro Prozess erkannt: Docker (inkl. Name), Podman, **Proxmox-LXC (`CT 101 (hostname)`) und -VMs (`VM 105 (name)`)**, libvirt, k8s; in der Detailansicht und im Filter suchbar |
| #119 | zram-Infos | ✅ zram-Geräte mit Original → komprimiert und Ratio in der Mem-Box |
| #185 | SIGSTOP/SIGCONT, auch für Kindprozesse | ✅ Taste `p`, Option „+Tree“ |
| #199 | Akkustand | ✅ in der CPU-Box-Titelleiste mit Lade-/Entladesymbol |
| – | Maussteuerung (in bpytop nachgereicht) | ✅ |
| #230 | Weitere Themes | ✅ lädt alle bashtop/bpytop/btop-`.theme`-Dateien unverändert, 9 Original-Themes liegen bei |
| #93 | Box mit eigenem Kommando (tail -f …) | ❌ nicht umgesetzt |
| #231 | Ausgabe auf freie VT umleiten | ❌ nicht nötig: `openvt -s -- rbtop` |
| #235 | Windows | ❌ (Linux-only, liest `/proc` und `/sys`) |

Nicht betroffen von Original-Bug #234 (Hänger bei `°C` in der sensors-Ausgabe), weil
rbtop die hwmon-Werte direkt aus `/sys` liest statt `sensors` zu parsen.

## Build & Installation

Linux, Rust ≥ 1.77:

```sh
cargo build --release          # → target/release/rbtop
sudo make install              # /usr/local/bin/rbtop + /usr/local/share/rbtop/themes
```

Fertige Binaries (x86_64 und aarch64, glibc und statisch mit musl) baut die
GitHub-Action bei jedem Tag `v*` und hängt sie an das Release.

## Konfiguration

`~/.config/rbtop/rbtop.cfg`, Format wie bashtop (`key="value"`). Gibt es noch keine,
wird eine vorhandene `~/.config/bashtop/bashtop.cfg` übernommen. Alles ist auch im
Optionsmenü (`o`) änderbar und wird beim Schließen gespeichert.

Original-Optionen: `color_theme`, `update_ms`, `proc_sorting`, `proc_reversed`,
`proc_tree`, `check_temp`, `draw_clock`, `background_update`, `custom_cpu_name`,
`proc_gradient`, `proc_per_core`, `disks_filter` (zusätzlich mit `exclude=`-Präfix),
`hires_graphs`.

Neu: `mem_graphs`, `show_swap`, `show_battery`, `show_gpu`, `show_containers`,
`net_device`, `mouse`.

Themes werden gesucht in `~/.config/rbtop/themes`, den Theme-Ordnern von
bashtop/bpytop/btop, `/usr/{local/,}share/{rbtop,bashtop,bpytop,btop}/themes` und
`themes/` neben der Binary.

## Getestet / nicht getestet

Getestet in einem echten PTY: alle Tasten, Maus, Resize, 80×24 und die
„Terminal size too small“-Meldung, Layout mit 2–128 Threads, Filter, Baum,
Detailansicht, Signale inkl. Stop/Continue, Optionen, Themes, Config-Speicherung.
Unit-Tests für Tastatur-Parser, cgroup→Container-Erkennung und Theme-Parser.

Mangels Hardware in der Build-Umgebung **nicht live getestet**: GPU-Anzeige, Akku,
Temperatursensoren und die Docker/Proxmox-Namensauflösung (der cgroup-Parser ist
per Unit-Test abgedeckt, die Namen kommen aus `/var/lib/docker/containers/*/config.v2.json`
bzw. `/etc/pve/{lxc,qemu-server}/*.conf` und brauchen root).

## Lizenz

Apache-2.0 wie das Original. Themes aus dem bashtop-Repository.
