# Changelog

All notable changes to rbtop. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Fixed
- Typing into the process filter puts the selection on the first row of the
  filtered list explicitly; before, it relied on the old top process staying
  on top, which an update tick between two key presses could change.

## [0.3.3] - 2026-11-28

Second half of the code audit: the low-priority findings and clean-ups.

### Fixed
- Kill dialog: a process that exited (or whose pid was reused by a new
  process) between opening the dialog and confirming it is no longer
  signalled; the dialog says "Process is gone" instead. Checked by start time.
- "+Tree" can no longer loop forever on a racy parent/child cycle, and collects
  the tree in one pass instead of one scan of all processes per child.
- Clicking a button while typing a filter no longer appends the button's
  letter to the filter; the filter input ends and the button acts.
- The battery tag no longer overlaps the clock on narrow terminals (80 columns):
  its meter shrinks or goes, and the tag is left out before it would overlap.
- Combining marks and other zero-width characters no longer shift process rows.
  Decomposed Latin letters (macOS keeps names in NFD) are composed, "U" + "¨"
  shows as "Ü".
- Linux: on systems with two CPU sockets the per-core temperatures of the second
  socket showed the first socket's values (core ids repeat per socket).
- The config is written to a temporary file and renamed, so it can no longer be
  left truncated. Values may contain `"` now.
- macOS: a USB disk plugged in under the diskN of a previously removed one shows
  disk io again.
- nvidia-smi is restarted with the new interval when the update time changes.
- macOS: the HID temperature fallback averages the sensors like the SMC path.
- `--help` names the right place of the `-d` timings.

### Changed
- CI: every release archive comes with a `.sha256` file, GitHub Actions are
  pinned to commit hashes, cross is pinned to 0.2.5, and a separate clippy job
  (`-D warnings`, Linux and macOS) runs on every push.
- Minimum Rust version declared in Cargo.toml (`rust-version = "1.77"`).
- No more clippy warnings.

## [0.3.2] - 2026-11-28

Security and robustness fixes from a full code audit.

### Security
- Linux: a process whose name is not valid UTF-8 (set with `prctl(PR_SET_NAME)`)
  was missing from the process list entirely, so a process could hide itself.
  Names are now parsed as bytes and shown with `�` for invalid bytes.
- pid 0 and pid 1 are never signalled, also not via "+Tree". On macOS
  `kernel_task` is pid 0 and `kill(0, …)` would hit rbtop's own process group;
  signalling init/systemd/launchd takes the whole system down.
- C1 control characters (e.g. U+009B, a one-byte CSI on some terminals), bidi
  overrides and zero-width characters in process names and command lines are
  replaced before they reach the terminal.

### Fixed
- Escape sequences split across reads (slow ssh links) no longer turn into
  Esc plus stray characters: an arrow key could open the menu, a mouse click
  could type `;15M` into the filter. A lone Esc counts as Esc after 50 ms.
- The process selection follows its pid when the list is re-sorted, instead of
  staying on the same row (and silently pointing at another process).
- nvidia-smi is started with default signal handling, so it exits together with
  rbtop (it inherited Rust's ignored SIGPIPE and could keep running). Quitting
  no longer hangs on a stuck nvidia-smi (SIGKILL after 500 ms).
- `sudo rbtop` with the user's HOME (the macOS default) no longer leaves a
  root-owned `~/.config/rbtop` behind that later normal runs cannot write.
  An existing one has to be fixed once with `sudo chown -R "$USER" ~/.config/rbtop`.
- Linux: USB drives mounted by udisks under `/run/media/<user>/` are shown again.
- Linux: pause/resume works for processes with non-UTF-8 names.

### Documentation
- README in English, German version in `README.de.md`.
- Benchmark rbtop vs. bashtop with chart, scripts and raw data (`bench/`).

## [0.3.1] - 2026-11-27

### Fixed
- macOS: no phantom "BAT 0%" on desktop Macs (Mac mini, Studio), which publish
  a battery service with `BatteryInstalled = No`.
- macOS: only an evenly spread subset of at most 6 cpu / 3 gpu SMC temperature
  sensors is read per tick; collecting took ~17 ms per tick on an M4 before.

## [0.3.0] - 2026-11-27

### Added
- macOS: cpu and gpu temperatures from the SMC (Apple Silicon and Intel), with
  the IOHID sensors as fallback.
- macOS: disk io rates per volume (APFS volumes are mapped to their physical disk).
- macOS: gpu load from IOAccelerator.
- macOS: battery status.

## [0.2.1] - 2026-11-27

### Fixed
- The Arguments column split the command line at the first space, so
  `/Applications/Google Chrome.app/…` showed `Chrome.app/…` as arguments.
  Program and arguments are now split at argv[0] (Linux and macOS).
- macOS: `/System/Volumes/Data` is no longer listed; it only repeated the root line.

## [0.2.0] - 2026-11-27

### Added
- macOS port: same UI, keys and config. Cpu, memory (Activity Monitor
  definitions), swap, disks, network and processes via native APIs.
- Release binaries for macOS: aarch64, x86_64 and universal.

### Changed
- Input waits with `select()` instead of `poll()` (macOS `poll()` does not support ttys).
- User names via `getpwuid_r`, so LDAP/sssd users are resolved on Linux too.
- `make install` works with BSD install (no `install -D`).

## [0.1.3] - 2026-11-26

### Added
- The sorted column header is highlighted, with ▲/▼ for the direction.

### Changed
- Generic virtual CPU models are shortened: "QEMU vCPU", "KVM vCPU".

## [0.1.2] - 2026-11-26

### Changed
- Readable disk names: OpenMediaVault's `/srv/dev-disk-by-label-X` shows as `X`,
  `dev-disk-by-uuid-…` as `uuid-<8 chars>`; names may use up to 20 columns.
- A device mounted several times (bind mounts) is listed once, under the nicest mountpoint.
- The `-d` timings moved to the cpu box border instead of covering a process row.

## [0.1.1] - 2026-11-26

### Added
- Click a process column header to sort by it, click again to reverse.

## [0.1.0] - 2026-11-25

### Added
- First release: bashtop rewritten in Rust with the same layout, key bindings,
  config and theme format, plus the feasible feature requests from the bashtop
  issue tracker (#17, #26/#221, #48, #63, #73, #97, #119, #185, #199).

[Unreleased]: https://github.com/gbomacfly/rbtop/compare/v0.3.3...HEAD
[0.3.3]: https://github.com/gbomacfly/rbtop/compare/v0.3.2...v0.3.3
[0.3.2]: https://github.com/gbomacfly/rbtop/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/gbomacfly/rbtop/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/gbomacfly/rbtop/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/gbomacfly/rbtop/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/gbomacfly/rbtop/compare/v0.1.3...v0.2.0
[0.1.3]: https://github.com/gbomacfly/rbtop/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/gbomacfly/rbtop/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/gbomacfly/rbtop/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/gbomacfly/rbtop/releases/tag/v0.1.0
