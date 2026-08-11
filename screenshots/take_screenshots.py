#!/usr/bin/env python3
"""
Screenshot generator for LR — Log Reader.

Captures the TUI by running lr in a pseudo-terminal (PTY) and recording
the terminal output. Produces both a raw ANSI dump and a plain text dump.

Requirements:
    - Python 3 (standard library only)
    - lr binary built (cargo build --release)

Usage:
    python3 screenshots/take_screenshots.py

Output:
    screenshots/*.ans  — raw ANSI terminal output (can be replayed with `cat`)
    screenshots/*.txt  — plain text version (control chars stripped)

To convert ANSI to PNG, use one of:
    - ansi2image (npm install -g ansi2image)
    - svg-bash-render + rsvg-convert
    - Take a screenshot of `cat screenshots/foo.ans` in your terminal
"""

import os
import pty
import select
import struct
import subprocess
import time
import fcntl
import termios
import re

# Configuration
LR_BINARY = os.environ.get("LR_BINARY", "target/release/lr")
SCREENSHOT_DIR = os.path.dirname(os.path.abspath(__file__))
PROJECT_DIR = os.path.dirname(SCREENSHOT_DIR)
TERM_WIDTH = 120
TERM_HEIGHT = 40
CAPTURE_TIMEOUT = 8.0  # hard timeout per screenshot

# Sample log files to generate
SAMPLES = {
    "syslog": """Jan 15 10:23:45 web01 sshd[1234]: Accepted publickey for admin from 10.0.0.5
Jan 15 10:23:46 web01 systemd[1]: Started nginx.service.
Jan 15 10:23:47 web01 nginx[2345]: 10.0.0.5 - - [15/Jan/2026:10:23:47 +0000] "GET /api/health HTTP/1.1" 200 15
Jan 15 10:23:48 web01 sshd[1235]: Failed password for root from 185.220.101.1
Jan 15 10:23:49 web01 kernel: [12345.67] EXT4-fs warning: check_fs_bitmaps: ...
Jan 15 10:23:50 web01 app[3000]: ERROR Database connection timeout after 30s
Jan 15 10:23:51 web01 app[3000]: WARN Retrying connection (attempt 2/3)
Jan 15 10:23:52 web01 app[3000]: INFO Connection restored
Jan 15 10:23:53 web01 app[3000]: DEBUG Query: SELECT * FROM users WHERE active=1
Jan 15 10:23:54 web01 app[3000]: TRACE cache.get("user:42") -> miss
""",
    "jsonl": """{"ts":"2026-01-15T10:23:45Z","level":"info","msg":"Server started","port":8080}
{"ts":"2026-01-15T10:23:46Z","level":"debug","msg":"Loading config","path":"/etc/app.toml"}
{"ts":"2026-01-15T10:23:47Z","level":"warn","msg":"High memory usage","rss_mb":512}
{"ts":"2026-01-15T10:23:48Z","level":"error","msg":"Request failed","status":500,"path":"/api/users"}
{"ts":"2026-01-15T10:23:49Z","level":"info","msg":"Request completed","status":200,"duration_ms":15}
{"ts":"2026-01-15T10:23:50Z","level":"debug","msg":"Cache stats","hits":1024,"misses":3}
{"ts":"2026-01-15T10:23:51Z","level":"trace","msg":"GC pause","duration_us":120}
""",
    "clf": """10.0.0.5 - admin [15/Jan/2026:10:23:45 +0000] "GET /index.html HTTP/1.1" 200 1024 "https://example.com" "Mozilla/5.0"
10.0.0.5 - - [15/Jan/2026:10:23:46 +0000] "POST /api/login HTTP/1.1" 401 89 "-" "curl/8.0"
10.0.0.6 - alice [15/Jan/2026:10:23:47 +0000] "GET /api/users?page=1 HTTP/1.1" 200 4096 "https://app.example.com" "Mozilla/5.0"
10.0.0.7 - - [15/Jan/2026:10:23:48 +0000] "GET /favicon.ico HTTP/1.1" 404 64 "-" "Mozilla/5.0"
10.0.0.5 - admin [15/Jan/2026:10:23:49 +0000] "PUT /api/settings HTTP/1.1" 200 51 "https://app.example.com/settings" "Mozilla/5.0"
""",
    "logfmt": """ts=2026-01-15T10:23:45Z level=info msg="Server started" port=8080
ts=2026-01-15T10:23:46Z level=debug msg="Loading config" path=/etc/app.toml
ts=2026-01-15T10:23:47Z level=warn msg="High memory usage" rss_mb=512
ts=2026-01-15T10:23:48Z level=error msg="Request failed" status=500 path=/api/users
ts=2026-01-15T10:23:49Z level=info msg="Request completed" status=200 duration_ms=15
ts=2026-01-15T10:23:50Z level=debug msg="Cache stats" hits=1024 misses=3
""",
}


def set_pty_size(master, rows, cols):
    """Set the PTY window size."""
    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))


def strip_ansi(text):
    """Remove ANSI escape sequences for plain text output."""
    return re.sub(r"\x1b\[[0-9;]*[A-Za-z]", "", text)


def capture_tui(args, duration=2.0, keys=None, name="screenshot"):
    """Run lr in a PTY and capture the terminal output.

    Args:
        args: command-line arguments for lr
        duration: how long to run before capturing
        keys: list of (bytes, delay_after) tuples to send as keystrokes
        name: output filename base
    """
    start_time = time.time()

    def time_left():
        return max(0, CAPTURE_TIMEOUT - (time.time() - start_time))

    master, slave = pty.openpty()
    set_pty_size(master, TERM_HEIGHT, TERM_WIDTH)

    cmd = [LR_BINARY] + args
    proc = subprocess.Popen(
        cmd,
        stdin=slave,
        stdout=slave,
        stderr=slave,
        cwd=PROJECT_DIR,
    )
    os.close(slave)

    output = b""

    def drain_and_respond(timeout_s):
        """Read available data and respond to cursor position queries."""
        nonlocal output
        deadline = time.time() + timeout_s
        while time.time() < deadline and time_left() > 0:
            ready, _, _ = select.select([master], [], [], 0.05)
            if not ready:
                break
            try:
                data = os.read(master, 65536)
                if not data:
                    break
                output += data
                if b"\x1b[6n" in data:
                    os.write(master, b"\x1b[1;1R")
            except OSError:
                break

    # Initial render: wait and respond to cursor position queries.
    time.sleep(min(0.3, time_left()))
    drain_and_respond(0.5)

    # Send keystrokes if any
    if keys:
        for key, delay in keys:
            if time_left() <= 0:
                break
            try:
                os.write(master, key)
            except OSError:
                break
            time.sleep(min(delay, time_left()))
            drain_and_respond(0.1)

    # Wait for final render
    remaining = min(max(0.1, duration - 0.5), time_left())
    time.sleep(remaining)
    drain_and_respond(1.0)

    # Quit
    try:
        os.write(master, b"q")
        time.sleep(0.2)
        drain_and_respond(0.5)
    except OSError:
        pass

    proc.terminate()
    try:
        proc.wait(timeout=2.0)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()
    try:
        os.close(master)
    except OSError:
        pass

    # Save outputs
    ansi_path = os.path.join(SCREENSHOT_DIR, f"{name}.ans")
    txt_path = os.path.join(SCREENSHOT_DIR, f"{name}.txt")

    with open(ansi_path, "wb") as f:
        f.write(output)

    text = output.decode("utf-8", errors="replace")
    plain = strip_ansi(text)
    with open(txt_path, "w") as f:
        f.write(plain)

    print(f"  Saved {name}.ans ({len(output)} bytes) and {name}.txt")
    return ansi_path


def generate_sample_logs():
    """Generate sample log files for screenshots."""
    sample_dir = os.path.join(SCREENSHOT_DIR, "samples")
    os.makedirs(sample_dir, exist_ok=True)

    paths = {}
    for name, content in SAMPLES.items():
        # Repeat content to get enough lines
        lines = content.strip().split("\n")
        full = "\n".join(lines * 20) + "\n"
        path = os.path.join(sample_dir, f"{name}.log")
        with open(path, "w") as f:
            f.write(full)
        paths[name] = path

    return paths


def main():
    # Build release binary if needed
    lr_path = os.path.join(PROJECT_DIR, LR_BINARY)
    if not os.path.exists(lr_path):
        print("Building release binary...")
        subprocess.run(
            ["cargo", "build", "--release"],
            cwd=PROJECT_DIR,
            check=True,
        )

    print("Generating sample logs...")
    samples = generate_sample_logs()

    screenshots = [
        # (name, args, duration, keys)
        # keys are (bytes, delay_after) tuples
        ("main_syslog", [samples["syslog"]], 2.0, None),
        ("main_jsonl", [samples["jsonl"]], 2.0, None),
        ("main_clf", [samples["clf"]], 2.0, None),
        ("main_logfmt", [samples["logfmt"]], 2.0, None),
        ("follow_mode", ["-f", samples["syslog"]], 2.0, None),
        ("line_numbers", [samples["syslog"]], 2.0, [(b"l", 0.5)]),
        ("end_view", [samples["syslog"]], 2.0, [(b"\x1b[4~", 0.5)]),  # PgDn
        ("search", [samples["syslog"]], 3.0, [(b"/", 0.3), (b"ERROR", 0.5), (b"\r", 0.5)]),
        ("severity_filter", [samples["syslog"]], 2.0, [(b"4", 0.3), (b"5", 0.3)]),
    ]

    print(f"Taking {len(screenshots)} screenshots...")
    for name, args, duration, keys in screenshots:
        print(f"  Capturing {name}...")
        try:
            capture_tui(args, duration=duration, keys=keys, name=name)
        except Exception as e:
            print(f"  ERROR capturing {name}: {e}")

    print(f"\nDone! Screenshots saved to {SCREENSHOT_DIR}/")
    print("To view: cat screenshots/<name>.ans")
    print("To convert to PNG, use ansi2image or take a terminal screenshot")


if __name__ == "__main__":
    main()
