#!/usr/bin/env python3
"""从 2048 主图生成 RustMC 启动器各平台图标资源。

用法：
    python3 tools/make_icons.py

输入：assets/icon/icon-2048.png（品牌主图）
输出：
    assets/icon/icon-{16,24,32,48,64,128,256}.png  各尺寸 PNG
    assets/icon/window-32.rgba                      32x32 RGBA 原始字节（供 eframe 窗口图标内嵌）
    assets/icon/rustmc.ico                          Windows 多尺寸图标
"""

from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent
ICON = ROOT / "assets" / "icon"
SRC = ICON / "icon-2048.png"

im = Image.open(SRC).convert("RGBA")
w, h = im.size

# 四角连通的白色背景抠成透明（白色字母 R 被绿色描边与方块包围，不与外围白连通，不会被误删）。
for corner in [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)]:
    ImageDraw.floodfill(im, corner, (0, 0, 0, 0), thresh=45)

# 各尺寸 PNG（LANCZOS 平滑缩放，标题栏与桌面均可辨识）。
for size in (16, 24, 32, 48, 64, 128, 256):
    im.resize((size, size), Image.LANCZOS).save(ICON / f"icon-{size}.png")

# 窗口标题栏 32x32 RGBA 原始字节，main.rs 通过 include_bytes! 内嵌。
r32 = im.resize((32, 32), Image.LANCZOS)
(ICON / "window-32.rgba").write_bytes(r32.tobytes())

# Windows 多尺寸 .ico。
im.resize((256, 256), Image.LANCZOS).save(
    ICON / "rustmc.ico",
    sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (256, 256)],
)

print(f"图标资源已生成到 {ICON}")
