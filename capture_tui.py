#!/usr/bin/env python3
"""通过 PTY 运行启动器，用 pyte 捕获终端画面，PIL 渲染成 PNG。"""
import fcntl
import os
import select
import struct
import sys
import termios
import time

import pyte
from PIL import Image, ImageDraw, ImageFont

COLS, ROWS = 100, 32
BIN = os.path.join(os.path.dirname(__file__), "target", "release", "rustmc-launcher")
OUT = sys.argv[1] if len(sys.argv) > 1 else "tui_main.png"
KEYS = sys.argv[2] if len(sys.argv) > 2 else ""
HOLD = float(sys.argv[3]) if len(sys.argv) > 3 else 7.0

# 16 色调色板（xterm 深色主题）
BASE16 = {
    "black": "#1a1b26",
    "red": "#f7768e",
    "green": "#9ece6a",
    "yellow": "#e0af68",
    "blue": "#7aa2f7",
    "magenta": "#bb9af7",
    "cyan": "#7dcfff",
    "white": "#c0caf5",
    "brightblack": "#565f89",
    "brightred": "#f7768e",
    "brightgreen": "#9ece6a",
    "brightyellow": "#e0af68",
    "brightblue": "#7aa2f7",
    "brightmagenta": "#bb9af7",
    "brightcyan": "#7dcfff",
    "brightwhite": "#ffffff",
}
DEFAULT_BG = "#1a1b26"
DEFAULT_FG = "#c0caf5"


def color(name, bold=False):
    if name == "default" or name is None:
        return DEFAULT_FG
    if name in BASE16:
        return BASE16[name]
    if name.isdigit():  # 256 色
        n = int(name)
        if n < 16:
            key = list(BASE16.keys())[n]
            return BASE16[key]
        if 16 <= n <= 231:
            n -= 16
            r0, g0, b0 = n // 36, (n // 6) % 6, n % 6
            cube = [0, 95, 135, 175, 215, 255]
            return "#%02x%02x%02x" % (cube[r0], cube[g0], cube[b0])
        v = 8 + (n - 232) * 10
        return "#%02x%02x%02x" % (v, v, v)
    return DEFAULT_FG


REG_FONT = "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"
BOLD_FONT = "/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc"
FONT_INDEX = 7  # Noto Sans Mono CJK SC


def find_font():
    if os.path.exists(REG_FONT):
        return REG_FONT
    return None


def main():
    screen = pyte.Screen(COLS, ROWS)
    stream = pyte.Stream(screen)

    pid, fd = os.forkpty()
    if pid == 0:
        os.environ["TERM"] = "xterm-256color"
        os.execv(BIN, [BIN])
        os._exit(1)

    # 设置 pty 窗口大小
    fcntl.ioctl(
        fd,
        termios.TIOCSWINSZ,
        struct.pack("HHHH", ROWS, COLS, 0, 0),
    )

    start = time.time()
    keys_sent = False
    while time.time() - start < HOLD:
        r, _, _ = select.select([fd], [], [], 0.2)
        if r:
            try:
                data = os.read(fd, 65536)
                if not data:
                    break
                stream.feed(data.decode("utf-8", "replace"))
            except OSError:
                break
        if KEYS and not keys_sent and time.time() - start > 3.5:
            for k in KEYS:
                os.write(fd, k.encode())
            keys_sent = True
    try:
        os.kill(pid, 9)
    except OSError:
        pass
    os.waitpid(pid, 0)

    render(screen, OUT)
    print(f"已保存 {OUT}")


def render(screen, out):
    font_path = find_font()
    if font_path:
        font = ImageFont.truetype(font_path, 16, index=FONT_INDEX)
        bold = ImageFont.truetype(BOLD_FONT, 16, index=FONT_INDEX)
    else:
        font = bold = ImageFont.load_default()
    cw = 10
    ch = 19
    W, H = COLS * cw + 16, ROWS * ch + 12
    img = Image.new("RGB", (W, H), DEFAULT_BG)
    d = ImageDraw.Draw(img)

    for row in range(ROWS):
        for col in range(COLS):
            c = screen.buffer[row][col]
            ch_ = c.data
            # 空格，以及宽字符的第二占位（pyte 用空串）
            if ch_ in (" ", ""):
                continue
            fg = color(c.fg, c.bold)
            bg = color(c.bg) if c.bg not in ("default", None) else DEFAULT_BG
            if c.reverse:
                fg, bg = bg, fg
            x, y = 8 + col * cw, 6 + row * ch
            # CJK 全角字符占两列，背景画两格
            wide = len(ch_.encode("utf-8")) > 1 and _is_wide(ch_)
            span = 2 if wide else 1
            if bg != DEFAULT_BG:
                d.rectangle(
                    [x - 1, y - 1, x + cw * span - 1, y + ch - 2], fill=bg
                )
            f = bold if c.bold else font
            d.text((x, y), ch_, fill=fg, font=f)
    img.save(out)


def _is_wide(s):
    import unicodedata
    return unicodedata.east_asian_width(s[0]) in ("W", "F")


if __name__ == "__main__":
    main()
