"""Benchmark a TUI system monitor in a pty.

Measures, for one run:
  startup   time from exec to the first complete frame (output quiet for 300 ms
            after the box title "processes" appeared)
  cpu       user+sys CPU of the whole process tree (incl. exited children) over the window
  rss       mean / max resident memory of the process tree
  forks     new processes created system-wide during the window (minus idle baseline)
  output    bytes/s written to the terminal
  latency   time from a key press (Down) to the first byte of the redraw, median of N

Usage: bench.py NAME SECONDS -- argv...
Prints one JSON line.
"""
import json, os, pty, select, signal, struct, sys, termios, fcntl, time, statistics

COLS, ROWS = 160, 45
TCK = os.sysconf("SC_CLK_TCK")
PAGE = os.sysconf("SC_PAGE_SIZE")

name, secs = sys.argv[1], float(sys.argv[2])
argv = sys.argv[sys.argv.index("--") + 1:]


def forks_total():
    with open("/proc/stat") as f:
        for l in f:
            if l.startswith("processes "):
                return int(l.split()[1])
    return 0


def children_map():
    kids = {}
    for d in os.listdir("/proc"):
        if not d.isdigit():
            continue
        try:
            with open(f"/proc/{d}/stat") as f:
                s = f.read()
            pp = int(s[s.rfind(")") + 2:].split()[1])
            kids.setdefault(pp, []).append(int(d))
        except Exception:
            pass
    return kids


def tree(root):
    kids, out, todo = children_map(), [], [root]
    while todo:
        p = todo.pop()
        out.append(p)
        todo += kids.get(p, [])
    return out


def proc_ticks(p, with_children):
    with open(f"/proc/{p}/stat") as f:
        s = f.read()
    v = s[s.rfind(")") + 2:].split()
    t = int(v[11]) + int(v[12])
    if with_children:
        t += int(v[13]) + int(v[14])  # reaped children (bashtop forks a lot)
    return t


def tree_cpu(root):
    total = 0
    for p in tree(root):
        try:
            # cutime/cstime only for the root: otherwise reaped grandchildren count twice
            total += proc_ticks(p, p == root)
        except Exception:
            pass
    return total


def tree_rss(root):
    total = 0
    for p in tree(root):
        try:
            with open(f"/proc/{p}/statm") as f:
                total += int(f.read().split()[1]) * PAGE
        except Exception:
            pass
    return total


t_exec = time.time()
pid, fd = pty.fork()
if pid == 0:
    os.environ["TERM"] = "xterm-256color"
    os.execvp(argv[0], argv)
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))

out_bytes = 0
buf = b""


def read_for(t, stop_on_first=False):
    """Read output for up to t seconds. Returns time of first byte (or None)."""
    global out_bytes, buf
    end = time.time() + t
    first = None
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], max(0, end - time.time()))
        if not r:
            continue
        try:
            d = os.read(fd, 1 << 16)
        except OSError:
            break
        if not d:
            break
        out_bytes += len(d)
        if len(buf) < 4 << 20:
            buf += d
        if first is None:
            first = time.time()
            if stop_on_first:
                return first
    return first


# ---- startup: first complete frame
startup = None
deadline = time.time() + 30
last_data = None
while time.time() < deadline:
    r, _, _ = select.select([fd], [], [], 0.05)
    if r:
        try:
            d = os.read(fd, 1 << 16)
        except OSError:
            break
        out_bytes += len(d)
        buf += d
        last_data = time.time()
    elif last_data and b"processes" in buf and time.time() - last_data > 0.3:
        startup = last_data - t_exec
        break

# settle (bashtop's first samples, history)
read_for(6)

# ---- steady state window
f0, c0, b0, t0 = forks_total(), tree_cpu(pid), out_bytes, time.time()
rss = []
nprocs = []
while time.time() - t0 < secs:
    read_for(1.0)
    rss.append(tree_rss(pid))
    nprocs.append(sum(1 for d in os.listdir("/proc") if d.isdigit()))
f1, c1, b1, t1 = forks_total(), tree_cpu(pid), out_bytes, time.time()
wall = t1 - t0

# ---- key latency: Down, measure first redraw byte. Between presses wait for quiet.
lat = []
for i in range(15):
    read_for(0.4)  # drain
    t = time.time()
    os.write(fd, b"\x1b[B")
    first = read_for(2.0, stop_on_first=True)
    if first:
        lat.append(first - t)

# quit
try:
    os.write(fd, b"q")
    read_for(1.0)
except OSError:
    pass
try:
    os.kill(pid, signal.SIGKILL)
except Exception:
    pass
try:
    os.waitpid(pid, 0)
except Exception:
    pass

print(json.dumps({
    "name": name,
    "startup_s": round(startup, 3) if startup else None,
    "cpu_pct_of_core": round(100 * (c1 - c0) / TCK / wall, 3),
    "rss_mean_mib": round(statistics.mean(rss) / 2**20, 1) if rss else None,
    "rss_max_mib": round(max(rss) / 2**20, 1) if rss else None,
    "forks_per_s": round((f1 - f0) / wall, 2),
    "out_kib_s": round((b1 - b0) / wall / 1024, 2),
    "latency_ms_median": round(statistics.median(lat) * 1000, 1) if lat else None,
    "latency_ms_max": round(max(lat) * 1000, 1) if lat else None,
    "latency_n": len(lat),
    "window_s": round(wall, 1),
    "procs_min": min(nprocs) if nprocs else None,
    "procs_max": max(nprocs) if nprocs else None,
}))
