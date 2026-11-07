#!/usr/bin/env bash
# RustMC Launcher —— macOS 一键构建脚本
#
# 在任意一台 Mac 上运行本脚本即可生成可直接双击运行的
# 「RustMC Launcher.app」（带应用图标）。
#
# 用法：
#   1. 把整个源码文件夹（包含本脚本的文件夹）放到 Mac 上
#   2. 打开「终端」，cd 到该文件夹
#   3. 执行：  bash build-macos.sh
#
# 说明：
#   - 若尚未安装 Rust，脚本会自动通过 rustup 安装（需要联网，可能要求输入密码）。
#   - 同时为 Intel 芯片（x86_64）与 Apple 芯片（M1/M2/M3）构建通用二进制。
#   - 产物位于当前目录： RustMC Launcher.app
#
set -euo pipefail

cd "$(dirname "$0")"

echo "==> 检查 Rust 工具链 …"
if ! command -v cargo >/dev/null 2>&1; then
    echo "未检测到 Rust，正在通过 rustup 安装 …"
    /bin/bash -c "$(curl -fsSL https://sh.rustup.rs)" -s -- -y
fi
export PATH="$HOME/.cargo/bin:$PATH"

echo "==> 添加 macOS 编译目标 …"
rustup target add x86_64-apple-darwin aarch64-apple-darwin

echo "==> 编译 Apple 芯片（arm64）版本 …"
cargo build --release --target aarch64-apple-darwin

echo "==> 编译 Intel 芯片（x86_64）版本 …"
cargo build --release --target x86_64-apple-darwin

echo "==> 合并为通用二进制（Universal Binary） …"
APP="RustMC Launcher.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
lipo -create -output "$APP/Contents/MacOS/RustMC-Launcher" \
    "target/aarch64-apple-darwin/release/rustmc-launcher" \
    "target/x86_64-apple-darwin/release/rustmc-launcher"
chmod +x "$APP/Contents/MacOS/RustMC-Launcher"

# 应用图标放入 Resources（与 Info.plist 的 CFBundleIconFile 对应）。
if [ -f "assets/icon/icon-256.png" ]; then
    cp "assets/icon/icon-256.png" "$APP/Contents/Resources/icon.png"
fi

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>RustMC Launcher</string>
    <key>CFBundleDisplayName</key>
    <string>RustMC Launcher</string>
    <key>CFBundleIdentifier</key>
    <string>com.rustmc.launcher</string>
    <key>CFBundleVersion</key>
    <string>0.1.0</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleExecutable</key>
    <string>RustMC-Launcher</string>
    <key>CFBundleIconFile</key>
    <string>icon.png</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
PLIST

echo ""
echo "==> 完成！已生成： $PWD/$APP"
echo "    把它拖到「应用程序」即可。首次打开若提示“无法验证开发者”，"
echo "    请在「系统设置 - 隐私与安全性」里点击「仍要打开」。"
