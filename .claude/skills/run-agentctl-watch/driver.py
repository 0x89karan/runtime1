#!/usr/bin/env python3
"""Drive `agentctl watch` (a ratatui TUI) from an agent, and capture readable frames.

Why not tmux: macOS dev boxes here don't have it, and a pty gives exact control over the
window size, which this TUI is sensitive to. Stdlib only — no pip install.

    driver.py --frames OUT_DIR -- ./target/debug/agentctl watch --url http://127.0.0.1:7999 --no-plain

Steps are given with --step, in order. Each is one of:
    wait:SECONDS        let the app run (and keep draining its output)
    key:LITERAL         send raw keys, e.g. key:l   key:qq   key:/error   key:\r  key:\t  key:\x1b
    snap:NAME           capture a FULL frame to OUT_DIR/NAME.txt
    size:ROWSxCOLS      resize the terminal (exercises the layout guards)

Three details that make or break this harness — all learned the hard way:

1. **Set the window size.** Without `TIOCSWINSZ` the pty is 0x0 and ratatui renders an EMPTY
   frame, so every assertion silently "passes" against a blank screen.
2. **Force a repaint before capturing.** ratatui writes only changed cells, so a mid-session
   capture is a diff, not a screen. Toggling the width by one column makes crossterm emit
   `Event::Resize`, which repaints everything — that is what `snap` does.
3. **Use `waitpid(WNOHANG)`, never `kill(pid, 0)`, to test for exit.** `kill(pid, 0)` succeeds
   on a ZOMBIE, so a clean exit reads as a hang. This cost two false "it won't quit" bug
   reports before it was understood.

Exit status: 0 if the app exited on its own (i.e. the quit keys worked), 1 if it had to be
killed. The frame files are written either way.
"""

import argparse
import fcntl
import os
import pty
import re
import select
import signal
import struct
import sys
import termios
import time

DEFAULT_ROWS, DEFAULT_COLS = 40, 140


class Pty:
    def __init__(self, argv, rows=DEFAULT_ROWS, cols=DEFAULT_COLS):
        self.rows, self.cols = rows, cols
        self.buf = bytearray()
        self.pid, self.fd = pty.fork()
        if self.pid == 0:  # child
            os.execvp(argv[0], argv)
        self.set_size(rows, cols)

    def set_size(self, rows, cols):
        self.rows, self.cols = rows, cols
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

    def pump(self, seconds):
        """Drain output for `seconds`. Draining matters: a full pty buffer blocks the app."""
        end = time.time() + seconds
        while time.time() < end:
            r, _, _ = select.select([self.fd], [], [], 0.05)
            if not r:
                continue
            try:
                chunk = os.read(self.fd, 1 << 20)
            except OSError:
                return
            if not chunk:
                return
            self.buf.extend(chunk)

    def keys(self, text):
        os.write(self.fd, text.encode())
        self.pump(0.2)

    def frame(self):
        """Force a full repaint, then render what was drawn into a text grid.

        The intended size is captured FIRST: `set_size` updates `self.cols`, so shrinking via
        `self.cols - 1` and then "restoring" with `self.cols` would set the same shrunken width
        twice — no size change, no `Event::Resize`, no repaint, and the capture comes back as a
        near-blank diff. (Found by running this skill's own recipe before committing it.)
        """
        rows, cols = self.rows, self.cols
        self.set_size(rows, cols - 1)
        self.pump(0.4)
        self.buf.clear()
        self.set_size(rows, cols)
        self.pump(0.9)
        out = render(bytes(self.buf), rows, cols)
        self.buf.clear()
        return out

    def wait_for_exit(self, seconds=5.0):
        """True if the process exited on its own within `seconds`."""
        deadline = time.time() + seconds
        while time.time() < deadline:
            done, _ = os.waitpid(self.pid, os.WNOHANG)  # NOT kill(pid, 0) — see module docstring
            if done:
                return True
            self.pump(0.2)
        return False

    def kill(self):
        try:
            os.kill(self.pid, signal.SIGKILL)
            os.waitpid(self.pid, 0)
        except (OSError, ChildProcessError):
            pass


def render(data: bytes, rows: int, cols: int) -> str:
    """Minimal VT replay: honour CUP (`ESC[row;colH`), ED (`J`), EL (`K`) onto a character grid.

    Not a real terminal emulator — enough to read a ratatui frame as text. Note that a naive
    `grep` over raw output does NOT work: ratatui interleaves style escapes mid-line, so words
    get split. Always assert against this rendered grid.
    """
    grid = [[" "] * cols for _ in range(rows)]
    r = c = i = 0
    s = data.decode("utf8", "replace")
    while i < len(s):
        ch = s[i]
        if ch == "\x1b":
            m = re.match(r"\x1b\[([0-9;]*)([A-Za-z])", s[i:])
            if m:
                nums = [int(x) for x in m.group(1).split(";") if x != ""]
                fin = m.group(2)
                if fin == "H":
                    r = (nums[0] - 1) if nums else 0
                    c = (nums[1] - 1) if len(nums) > 1 else 0
                elif fin == "J":
                    grid = [[" "] * cols for _ in range(rows)]
                    r = c = 0
                elif fin == "K":
                    for x in range(c, cols):
                        grid[r][x] = " "
                i += m.end()
                continue
            m = re.match(r"\x1b\][^\x07\x1b]*(\x07|\x1b\\)", s[i:])  # OSC
            if m:
                i += m.end()
                continue
            i += 2
            continue
        if ch == "\n":
            r, c = min(r + 1, rows - 1), 0
        elif ch == "\r":
            c = 0
        else:
            if 0 <= r < rows and 0 <= c < cols and ch.isprintable():
                grid[r][c] = ch
            c += 1
            if c >= cols:
                c, r = 0, min(r + 1, rows - 1)
        i += 1
    return "\n".join("".join(row).rstrip() for row in grid)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--frames", required=True, help="directory for captured frames")
    ap.add_argument("--step", action="append", default=[], help="wait:S | key:K | snap:N | size:RxC")
    ap.add_argument("--rows", type=int, default=DEFAULT_ROWS)
    ap.add_argument("--cols", type=int, default=DEFAULT_COLS)
    ap.add_argument("argv", nargs=argparse.REMAINDER, help="-- COMMAND [ARGS...]")
    args = ap.parse_args()

    argv = [a for a in args.argv if a != "--"]
    if not argv:
        ap.error("pass the command after `--`")
    os.makedirs(args.frames, exist_ok=True)

    term = Pty(argv, args.rows, args.cols)
    for step in args.step:
        kind, _, val = step.partition(":")
        if kind == "wait":
            term.pump(float(val))
        elif kind == "key":
            term.keys(val.encode().decode("unicode_escape"))
        elif kind == "snap":
            path = os.path.join(args.frames, f"{val}.txt")
            with open(path, "w") as f:
                f.write(term.frame())
            print(f"frame: {path}")
        elif kind == "size":
            rows, _, cols = val.partition("x")
            term.set_size(int(rows), int(cols))
            term.pump(0.5)
        else:
            ap.error(f"unknown step: {step}")

    exited = term.wait_for_exit()
    if not exited:
        term.kill()
    print(f"exited_on_its_own: {exited}")
    return 0 if exited else 1


if __name__ == "__main__":
    sys.exit(main())
