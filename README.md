# RustMC Launcher

一个用 Rust（eframe/egui + tokio + reqwest）编写的跨平台 Minecraft 启动器，
带图形界面（GUI）与终端界面（TUI）。除基础的多账号管理（离线 / 微软正版 / 第三方，本地持久化）、
版本下载与启动外，本版本还包含模组加载器安装、内容（整合包/模组/资源包/光影/世界）下载导入、
自定义 Java 启动参数、可配置下载线程数、下载进度条、版本隔离与像素风应用图标。

## 功能

### 游戏页（Play）
- 顶部筛选：**全部 / 正式版 / 快照版 / 已安装**。
- 列表分两组展示：**已安装**（本地 `versions/` 目录中已登记的版本，绿色 ✓ 前缀，排在最前）
  与**在线版本**（未下载的官方版本，R/S 标记）。
- 「已安装」标签只列出本地版本，选中后可直接启动，不依赖在线清单（离线也能启动本地版本）。
- 详情面板显示版本类型、加载器（Forge/Fabric/NeoForge/Quilt/OptiFine）与安装状态
  （已安装可直接启动 / 客户端就绪 / 待下载）。
- 一键打开游戏目录。

### 账号管理（角色）
- 支持保存多个账号并在本地**永久保留**，重启启动器后自动恢复上次选中的账号。
- 三种类型：**离线**（任意玩家名，按官方离线算法生成 UUID）、**微软正版**（设备码登录，
  登录成功后资料与令牌保存到本地）、**第三方**（自定义玩家名，可填可选 UUID / 令牌 / 备注，
  用于第三方服务器账号）。
- 账号列表中点击即可**切换**当前账号，点「删除」可移除某个账号；切换与删除都会立即写回本地配置。
- 配置目录：`~/.config/rustmc-launcher/config.json`（Windows 为 `%APPDATA%\rustmc-launcher`，
  macOS 为 `~/Library/Application Support/rustmc-launcher`）。

### 应用图标
- 像素风 Minecraft 主题图标（草方块 + 字母 R），已内嵌到窗口标题栏与任务栏。
- `assets/icon/` 下保存各尺寸 PNG、Windows `.ico` 与 macOS 用图；
  `tools/make_icons.py` 从 2048 主图重新生成全部尺寸资源。

### 加载器页（Loaders）
支持一键安装五类模组加载器：
- **Fabric / Quilt**：直接从官方 meta 拉取 profile JSON 生成版本，库文件在首次启动时下载。
- **Forge / NeoForge**：参考 HMCL 实现**原生安装**（`src/core/installer.rs`）——解析官方
  安装器的 `install_profile.json`，复制内置库、下载处理器依赖、准备原版客户端，
  然后逐个执行 `processors`（解包、合并映射、打补丁）并按 `outputs` 的 SHA1 校验，
  全程细粒度进度、可重复安装。**不需要先运行官方启动器、不依赖 `launcher_profiles.json`。**
  旧版（1.12 及以前，无 `spec` 字段）自动回退到官方 `--installClient` 静默安装。
- **OptiFine**：默认走 BMCLAPI 镜像（官网兜底），随后弹出官方安装窗口，点击 `Install` 即可。

### 内容页（Content）
- 从 Modrinth 在线搜索并安装：**模组 / 资源包 / 光影 / 世界（数据包）/ 整合包**。
- 可按 Minecraft 版本与加载器过滤；选择项目版本后一键下载安装。
- 支持 **Modrinth `.mrpack` 整合包**：自动解析 `modrinth.index.json`，按 client 环境
  并发下载、复制 overrides，并**自动安装所需的 Fabric / Quilt / Forge / NeoForge 加载器**
  （失败时提示手动到加载器页安装）。
- 兼容 CurseForge 整合包（`manifest.json`）：在网络可访问 CurseForge 时尽力下载。
- 支持本地导入：把 `.jar/.zip/.mrpack` 拖拽到窗口，或粘贴路径点「导入」；
  世界压缩包会自动识别 `level.dat` 并解压到 `saves/`。
- 可查看并删除已安装内容。

### 设置页（Settings）
- **首次启动引导**：第一次打开会询问游戏数据目录，默认使用可见目录 `~/RustMC`
  （Windows `%APPDATA%\RustMC`、macOS `~/Library/Application Support/RustMC`），
  也可一键切到官方 `.minecraft` 兼容旧数据；选择后永久记住，不再询问。
- 游戏目录可在设置页随时修改，保存后立即生效。
- **下载线程数滑块 0–256**：`0 = 自动`（按 CPU 逻辑核数，自动限制在 4–64）；
  手动值限制在 1–256。
- 最小/最大内存（MB）滑块。
- 自定义 Java 路径（下拉会列出探测到的 Java，也可手填）。
- 自定义 **JVM 参数**与**游戏参数**（多行，支持引号分组）。
- 窗口宽高与全屏开关。
- **版本隔离（默认开启）**：每个版本使用独立目录 `versions/<版本号>/`，存档、模组、
  资源包、光影、配置按版本隔离存放，互不串档；关闭则所有版本共用游戏根目录。
  库文件、assets、版本 jar、natives 仍全局共享，不重复下载。
- 设置持久化在系统配置目录：`~/.config/rustmc-launcher/config.json`（与游戏目录解耦）。

### 进度条
- 启动游戏或下载/安装内容时，底部显示进度条与状态：已下载文件数、已下载 MB、
  当前正在下载的文件，以及加载器/整合包安装阶段提示。

## 编译运行

需要 Rust 工具链（edition 2024，建议 Rust 1.90+）。

```bash
# 图形界面
cargo run --release

# 终端界面
cargo run --release --bin tui
```

构建产物：`target/release/rustmc-launcher`（GUI）、`target/release/tui`（TUI）。

Linux 下 GUI 需要系统提供 WebKit/GTK 相关库（eframe 默认后端）。
Windows/macOS 直接 `cargo build --release` 即可。

## 网络说明
- 所有列表/下载请求均带自动重试（连接中断、TLS 中断、超时、429/5xx 会重试 4 次）。
- 默认遵循系统代理环境变量（`HTTP_PROXY/HTTPS_PROXY`）。
- 若处于代理对部分游戏站点不友好的网络环境，可清除代理变量后直连，或使用正常网络。

## 目录结构
- `src/core/config.rs`    设置持久化、线程数/参数解析
- `src/core/modloaders.rs` 五类加载器列表/安装、Java 版本选择、网络重试
- `src/core/installer.rs` Forge/NeoForge 原生安装器（Maven 解析、processor 编排、补丁）
- `src/core/content.rs`   Modrinth/CurseForge 搜索、下载、整合包与本地导入
- `src/core/download.rs`  任务收集、并发下载、校验与 natives 解压
- `src/core/launch.rs`    启动参数组装（含自定义 JVM/游戏参数、窗口/全屏）
- `src/core/manifest.rs`  版本清单与继承版本合并
- `src/core/java.rs`      Java 探测与版本选择
- `src/gui.rs`            四标签页图形界面（含账号管理面板）
- `src/bin/tui.rs`        终端界面
- `assets/icon/`          各尺寸图标 PNG、Windows `.ico`、窗口内嵌 RGBA
- `tools/make_icons.py`   从主图批量生成各平台图标资源
- `build-macos.sh`        macOS 一键构建脚本（生成带图标的 `.app`）
- `examples/`             开发/验证程序（Forge 原生安装、端到端下载、启动参数打印等）

### 验证原生安装
```bash
# 用真实 Forge 安装器在临时目录完整安装（需要联网）
cargo run --example forge_test -- \
  /path/to/forge-1.20.1-47.2.0-installer.jar /tmp/forge-test 1.20.1
# 设置 KEEP=1 可复用目录验证幂等性（已完成的 processor 会按 SHA1 跳过）
```
