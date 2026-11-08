//! egui 图形界面：游戏启动、加载器安装、内容管理与设置。

use std::path::{Path, PathBuf};
use std::time::Duration;

use egui::{
    Color32, Context, CornerRadius, Margin, Panel, ProgressBar, RichText, ScrollArea, Slider,
    Stroke, Theme, Vec2,
};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use rustmc_launcher::core::auth::{self, Account, AccountKind, DeviceCode};
use rustmc_launcher::core::config::{self, LauncherConfig};
use rustmc_launcher::core::content::{
    self, ContentKind, ProjectVersion, SearchHit, SearchResponse,
};
use rustmc_launcher::core::download::{self, DownloadEvent};
use rustmc_launcher::core::java;
use rustmc_launcher::core::launch;
use rustmc_launcher::core::manifest::{
    self, VersionDetail, VersionEntry, VersionManifest,
};
use rustmc_launcher::core::modloaders::{
    self, LoaderKind, LoaderVersion, OptiFineEntry,
};
use rustmc_launcher::core::paths::LauncherPaths;
use rustmc_launcher::core::rules::EnvInfo;
use rustmc_launcher::core::ai::{Message as AiMessage, ToolSpec};

/* ----------------------------- 主题与配色 ----------------------------- */

/// 统一的视觉配色（现代深色 + Minecraft 草绿强调）。
#[allow(dead_code)]
pub mod theme {
    use egui::Color32;
    /// 主背景（面板）
    pub const BG: Color32 = Color32::from_rgb(23, 26, 31);
    /// 窗口 / 卡片背景
    pub const CARD: Color32 = Color32::from_rgb(32, 36, 43);
    /// 更深一档的内嵌背景（输入框、滚动条）
    pub const INSET: Color32 = Color32::from_rgb(18, 20, 25);
    /// 悬浮 / 分组边框
    pub const LINE: Color32 = Color32::from_rgb(48, 54, 63);
    /// 主强调色（草绿）
    pub const ACCENT: Color32 = Color32::from_rgb(126, 201, 106);
    /// 次要强调（蓝青）
    pub const BLUE: Color32 = Color32::from_rgb(111, 183, 232);
    /// 主文字
    pub const TEXT: Color32 = Color32::from_rgb(214, 222, 232);
    /// 弱文字
    pub const TEXT_DIM: Color32 = Color32::from_rgb(139, 149, 163);
    /// 警告
    pub const WARN: Color32 = Color32::from_rgb(224, 175, 104);
    /// 错误
    pub const ERROR: Color32 = Color32::from_rgb(224, 108, 117);
}

/// 应用统一的深色主题（圆角、间距、按钮、面板、窗口）。
fn apply_theme(ctx: &Context) {
    // 默认使用深色主题
    let _ = ctx.set_theme(Theme::Dark);

    ctx.all_styles_mut(|style| {
        // ---- 间距与尺寸 ----
        style.spacing.item_spacing = Vec2::new(8.0, 8.0);
        style.spacing.button_padding = Vec2::new(12.0, 6.0);
        style.spacing.interact_size = Vec2::new(28.0, 28.0);
        style.spacing.slider_width = 180.0;
        style.spacing.window_margin = Margin::same(12);
        style.spacing.menu_margin = Margin::same(8);
        // 加宽滚动条，低配机/小屏幕更容易点
        style.spacing.scroll.bar_width = 12.0;
        style.spacing.scroll.handle_min_length = 50.0;

        // ---- 视觉 ----
        let v = &mut style.visuals;
        v.dark_mode = true;
        v.override_text_color = Some(theme::TEXT);
        v.panel_fill = theme::BG;
        v.window_fill = theme::CARD;
        v.window_stroke = Stroke::new(1.0, theme::LINE);
        v.window_corner_radius = CornerRadius::same(10);
        v.menu_corner_radius = CornerRadius::same(8);
        v.faint_bg_color = Color32::from_rgb(28, 32, 38);
        v.extreme_bg_color = theme::INSET;
        v.code_bg_color = Color32::from_rgb(28, 32, 38);
        v.hyperlink_color = theme::BLUE;
        v.warn_fg_color = theme::WARN;
        v.error_fg_color = theme::ERROR;
        v.button_frame = true;
        v.collapsing_header_frame = true;
        v.slider_trailing_fill = true;
        v.interact_cursor = Some(egui::CursorIcon::PointingHand);

        // ---- 按钮 / 控件各状态 ----
        let w = &mut v.widgets;
        // 非交互（窗口本体、标签底色）
        w.noninteractive.bg_fill = theme::CARD;
        w.noninteractive.weak_bg_fill = theme::CARD;
        w.noninteractive.bg_stroke = Stroke::new(1.0, theme::LINE);
        w.noninteractive.corner_radius = CornerRadius::same(8);
        w.noninteractive.fg_stroke = Stroke::new(1.0, theme::TEXT);
        // 静止
        w.inactive.bg_fill = Color32::from_rgb(45, 51, 62);
        w.inactive.weak_bg_fill = Color32::from_rgb(45, 51, 62);
        w.inactive.bg_stroke = Stroke::new(1.0, theme::LINE);
        w.inactive.corner_radius = CornerRadius::same(8);
        w.inactive.fg_stroke = Stroke::new(1.0, theme::TEXT);
        // 悬浮
        w.hovered.bg_fill = Color32::from_rgb(58, 74, 51);
        w.hovered.weak_bg_fill = Color32::from_rgb(58, 74, 51);
        w.hovered.bg_stroke = Stroke::new(1.0, theme::ACCENT);
        w.hovered.corner_radius = CornerRadius::same(8);
        w.hovered.fg_stroke = Stroke::new(1.0, theme::TEXT);
        // 按下（绿底白字；注意 strong 文字色取自这里，必须保持亮色）
        w.active.bg_fill = theme::ACCENT;
        w.active.weak_bg_fill = theme::ACCENT;
        w.active.bg_stroke = Stroke::new(1.0, theme::ACCENT);
        w.active.corner_radius = CornerRadius::same(8);
        w.active.fg_stroke = Stroke::new(1.2, Color32::WHITE);
        // 展开（下拉等）
        w.open = w.hovered;
    });
}

/* ----------------------------- 事件与状态 ----------------------------- */

enum GuiEvent {
    ManifestLoaded(VersionManifest),
    ManifestError(String),
    Status(String),
    Download(DownloadEvent),
    Install(modloaders::InstallMessage),
    DeviceCode(DeviceCode),
    Authed(Account),
    AuthError(String),
    ReadyToLaunch(Box<VersionDetail>, PathBuf),
    LaunchError(String),
    GameExited(Result<(), String>),
    LoaderVersions(LoaderKind, Vec<LoaderVersion>),
    OptiFineVersions(Vec<OptiFineEntry>),
    SearchResults(SearchResponse),
    ProjectVersions(Vec<ProjectVersion>),
    ActionDone(String),
    ActionError(String),
    Ai(AiUiEvent),
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Tab {
    Play,
    Loaders,
    Content,
    Ai,
    Settings,
}

/// 聊天界面里的一行显示。
#[derive(Clone)]
enum ChatLine {
    User(String),
    Assistant(String),
    ToolCall { name: String, args: String },
    ToolResult { name: String, ok: bool, summary: String },
    Notice(String),
}

/// AI 后台任务发回 UI 的事件。
enum AiUiEvent {
    /// 追加一行聊天。
    Line(ChatLine),
    /// 本轮 AI 回复结束（busy 解除）。
    Done,
}

#[derive(PartialEq, Eq)]
enum Overlay {
    None,
    DeviceLogin,
    Notice,
}

#[derive(Default, Clone)]
struct Progress {
    total_files: u64,
    completed_files: u64,
    total_bytes: u64,
    done_bytes: u64,
    current: String,
}

/// 加载器安装任务的参数（在主线程取出后 move 到后台）。
enum LoaderPayload {
    Fabric(String, String),
    Quilt(String, String),
    Forge(String),
    NeoForge(String),
    OptiFine(OptiFineEntry),
}

/// 加载器页状态。
struct LoaderPage {
    kind: LoaderKind,
    mc_version: String,
    versions: Vec<LoaderVersion>,
    optifine: Vec<OptiFineEntry>,
    selected: Option<usize>,
    /// 记录当前版本列表对应的（类型, MC 版本），避免重复请求
    loaded_key: String,
}

impl Default for LoaderPage {
    fn default() -> Self {
        LoaderPage {
            kind: LoaderKind::Fabric,
            mc_version: String::new(),
            versions: Vec::new(),
            optifine: Vec::new(),
            selected: None,
            loaded_key: String::new(),
        }
    }
}

/// 内容页状态。
struct ContentPage {
    kind: ContentKind,
    /// 模组/整合包的加载器过滤（fabric / quilt / forge / neoforge）
    loader: String,
    query: String,
    results: Vec<SearchHit>,
    total: u32,
    offset: u32,
    selected_project: Option<String>,
    versions: Vec<ProjectVersion>,
    selected_version: Option<usize>,
    installed: Vec<String>,
    local_path: String,
    /// 当前 kind 是否已自动搜索过（首次进入/切换类型时自动搜全部）
    auto_loaded: bool,
}

impl Default for ContentPage {
    fn default() -> Self {
        ContentPage {
            kind: ContentKind::Mod,
            loader: "fabric".to_string(),
            query: String::new(),
            results: Vec::new(),
            total: 0,
            offset: 0,
            selected_project: None,
            versions: Vec::new(),
            selected_version: None,
            installed: Vec::new(),
            local_path: String::new(),
            auto_loaded: false,
        }
    }
}

pub struct GuiApp {
    _rt: tokio::runtime::Runtime,
    config: LauncherConfig,
    paths: LauncherPaths,
    client: reqwest::Client,
    env: EnvInfo,
    rx: UnboundedReceiver<GuiEvent>,
    tx: UnboundedSender<GuiEvent>,
    tab: Tab,
    overlay: Overlay,
    manifest: Option<VersionManifest>,
    shown: Vec<VersionEntry>,
    selected: Option<usize>,
    filter: u8,
    /// 本地保存的账号列表（离线 / 微软 / 第三方）。
    accounts: Vec<Account>,
    /// 当前生效账号。
    account: Option<Account>,
    /// 当前账号 UUID，便于持久化与切换。
    active_uuid: String,
    /// 添加账号表单字段。
    add_name: String,
    add_uuid: String,
    add_token: String,
    add_note: String,
    offline_name: String,
    status: String,
    notice: String,
    progress: Progress,
    busy: bool,
    device: Option<DeviceCode>,
    login_task: Option<JoinHandle<()>>,
    game_running: bool,
    loader_page: LoaderPage,
    content_page: ContentPage,
    /// 首次启动引导：是否正在询问游戏目录。
    first_run: bool,
    /// 首次引导里用户选择的游戏目录路径。
    first_run_path: String,
    /// 版本 id -> 所在的外部 .minecraft 根目录（空表示来自本启动器目录）。
    version_origins: std::collections::HashMap<String, std::path::PathBuf>,
    /// 设置页添加外部目录时的输入框。
    ext_dir_input: String,
    /// 缓存的 Java 检测结果，避免每次渲染设置页都扫描系统。
    cached_javas: Option<Vec<java::JavaInfo>>,
    /// 已加载成功的图标：url -> TextureHandle。
    icon_textures: std::collections::HashMap<String, egui::TextureHandle>,
    /// 正在后台下载中的图标 url。
    icon_loading: std::collections::HashSet<String>,
    /// 加载失败的图标 url 与上次失败时间，用于每秒重试。
    icon_failed: std::collections::HashMap<String, std::time::Instant>,
    /// 图标下载结果通道：(url, bytes)。
    icon_rx: Option<UnboundedReceiver<(String, Vec<u8>)>>,
    icon_tx: UnboundedSender<(String, Vec<u8>)>,
    /// AI 聊天：已显示的消息行。
    chat_lines: Vec<ChatLine>,
    /// AI 聊天：输入框内容。
    chat_input: String,
    /// AI 聊天：是否正在等待模型回复/执行工具。
    ai_busy: bool,
}

impl GuiApp {
    pub fn new(rt: tokio::runtime::Runtime, ctx: &Context) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let (icon_tx, icon_rx) = mpsc::unbounded_channel();
        let client = reqwest::Client::builder()
            .user_agent("RustMC-Launcher/0.1")
            .connect_timeout(std::time::Duration::from_secs(30))
            .pool_idle_timeout(std::time::Duration::from_secs(20))
            .build()
            .unwrap();

        let config = config::load();
        let paths = config.paths();
        let _ = paths.ensure_dirs();
        let env = EnvInfo::current();

        // 自动检测其他启动器目录（官方 / HMCL / PCL），合并到 external_dirs。
        let mut config = config;
        use rustmc_launcher::core::paths::detect_external_dirs;
        let detected = detect_external_dirs(&paths.base_dir);
        for d in detected {
            let s = d.display().to_string();
            if !config.external_dirs.contains(&s) {
                config.external_dirs.push(s);
            }
        }
        if !config.external_dirs.is_empty() {
            let _ = config::save(&config);
        }

        // 首次启动：若从未确认过游戏目录，进入引导选择目录。
        let first_run = !config.first_run_done;
        let first_run_path = paths.base_dir.display().to_string();

        // 从本地配置恢复已保存账号与上次选中的账号。
        let accounts = config.accounts.clone();
        let account = config.active().cloned();
        let active_uuid = config.active_account.clone();

        let app = GuiApp {
            _rt: rt,
            config,
            paths,
            client,
            env,
            rx,
            tx,
            tab: Tab::Play,
            overlay: Overlay::None,
            manifest: None,
            shown: Vec::new(),
            selected: None,
            filter: 0,
            accounts,
            account,
            active_uuid,
            add_name: String::new(),
            add_uuid: String::new(),
            add_token: String::new(),
            add_note: String::new(),
            offline_name: String::new(),
            status: String::new(),
            notice: String::new(),
            progress: Progress::default(),
            busy: false,
            device: None,
            login_task: None,
            game_running: false,
            loader_page: LoaderPage::default(),
            content_page: ContentPage::default(),
            first_run,
            first_run_path,
            version_origins: std::collections::HashMap::new(),
            ext_dir_input: String::new(),
            cached_javas: None,
            icon_textures: std::collections::HashMap::new(),
            icon_loading: std::collections::HashSet::new(),
            icon_failed: std::collections::HashMap::new(),
            icon_rx: Some(icon_rx),
            icon_tx,
            chat_lines: Vec::new(),
            chat_input: String::new(),
            ai_busy: false,
        };
        apply_theme(ctx);
        app.load_manifest(ctx);
        app
    }

    /* ----------------------------- 后台任务 ----------------------------- */

    fn load_manifest(&self, ctx: &Context) {
        let tx = self.tx.clone();
        let client = self.client.clone();
        let ctx = ctx.clone();
        self._rt.spawn(async move {
            match manifest::fetch_manifest(&client).await {
                Ok(m) => {
                    let _ = tx.send(GuiEvent::ManifestLoaded(m));
                }
                Err(e) => {
                    let _ = tx.send(GuiEvent::ManifestError(format!("{e:#}")));
                }
            }
            ctx.request_repaint();
        });
    }

    fn start_microsoft(&mut self, ctx: &Context) {
        let tx = self.tx.clone();
        let client = self.client.clone();
        let ctx = ctx.clone();
        let task = self._rt.spawn(async move {
            let device = match auth::request_device_code(&client).await {
                Ok(d) => d,
                Err(e) => {
                    let _ = tx.send(GuiEvent::AuthError(format!("{e:#}")));
                    ctx.request_repaint();
                    return;
                }
            };
            let device2 = device.clone();
            let _ = tx.send(GuiEvent::DeviceCode(device));
            ctx.request_repaint();

            let result = auth::complete_device_login(&client, device2, |_| {}).await;
            match result {
                Ok(a) => {
                    let _ = tx.send(GuiEvent::Authed(a));
                }
                Err(e) => {
                    let _ = tx.send(GuiEvent::AuthError(format!("{e:#}")));
                }
            }
            ctx.request_repaint();
        });
        self.login_task = Some(task);
        self.overlay = Overlay::DeviceLogin;
    }

    /* ----------------------------- 账号管理 ----------------------------- */

    /// 把账号列表与当前选中项写回配置并持久化到本地。
    fn persist_accounts(&mut self) {
        self.config.accounts = self.accounts.clone();
        self.config.active_account = self.active_uuid.clone();
        let _ = config::save(&self.config);
    }

    /// 当前活动游戏数据目录：隔离开启且游戏页已选中版本时，返回 `versions/<id>/`；
    /// 否则返回游戏根目录。内容页的模组/资源包/光影/世界都读写这个目录。
    /// 若选中版本来自外部启动器目录，则以该外部目录为根。
    fn active_game_dir(&self) -> std::path::PathBuf {
        let p = self.paths_for_selected();
        if self.config.version_isolation {
            if let Some(idx) = self.selected {
                if let Some(entry) = self.shown.get(idx) {
                    return p.version_dir(&entry.id);
                }
            }
        }
        p.base_dir.clone()
    }

    /// 后台下载图标图片字节，完成后通过 icon_tx 发回 UI 线程。
    fn spawn_icon_download(&mut self, url: &str) {
        if self.icon_loading.contains(url) {
            return;
        }
        self.icon_loading.insert(url.to_string());
        let url = url.to_string();
        let client = self.client.clone();
        let icon_tx = self.icon_tx.clone();
        self._rt.spawn(async move {
            match client.get(&url).send().await {
                Ok(resp) => match resp.bytes().await {
                    Ok(bytes) => {
                        let _ = icon_tx.send((url, bytes.to_vec()));
                    }
                    Err(_) => {
                        let _ = icon_tx.send((url, Vec::new()));
                    }
                },
                Err(_) => {
                    let _ = icon_tx.send((url, Vec::new()));
                }
            }
        });
    }

    /// 确保图标已加载：未加载则触发后台下载；失败由 logic 每秒自动重试。
    fn ensure_icon(&mut self, url: &str) {
        if self.icon_textures.contains_key(url) || self.icon_loading.contains(url) {
            return;
        }
        if self.icon_failed.contains_key(url) {
            return;
        }
        self.spawn_icon_download(url);
    }

    /// 渲染搜索结果左侧的小图标（28x28），未加载时显示占位方块。
    fn ui_icon(&mut self, ui: &mut egui::Ui, url: &Option<String>) {
        let size = egui::Vec2::new(28.0, 28.0);
        let tex = url.as_ref().and_then(|u| {
            self.ensure_icon(u);
            self.icon_textures.get(u).cloned()
        });
        if let Some(tex) = tex {
            ui.image((tex.id(), size));
            return;
        }
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(4),
            Color32::from_gray(60),
        );
    }
    fn ui_first_run(&mut self, ctx: &Context) {
        egui::Window::new("欢迎使用 RustMC")
            .collapsible(false)
            .resizable(false)
            .title_bar(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.vertical(|ui| {
                    ui.add_space(4.0);
                    ui.label(RichText::new("欢迎使用 RustMC").strong().size(20.0));
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(
                            "首次启动，请选择游戏数据目录（存档 / 模组 / 资源包 / 光影 / 配置）。",
                        )
                        .color(Color32::from_gray(180)),
                    );
                    ui.add_space(10.0);
                    ui.label("游戏目录：");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.first_run_path)
                            .desired_width(440.0),
                    );
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        let recommended = rustmc_launcher::core::paths::default_minecraft_dir()
                            .display()
                            .to_string();
                        if ui.button("推荐：可见目录 RustMC").clicked() {
                            self.first_run_path = recommended;
                        }
                        let legacy = rustmc_launcher::core::paths::legacy_minecraft_dir()
                            .display()
                            .to_string();
                        if ui.button("兼容官方 .minecraft").clicked() {
                            self.first_run_path = legacy;
                        }
                    });
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        let ok = egui::Button::new(
                            RichText::new("开始使用").size(16.0).color(Color32::BLACK),
                        )
                        .fill(Color32::from_rgb(158, 206, 106))
                        .min_size(egui::vec2(120.0, 30.0));
                        if ui.add(ok).clicked() {
                            self.confirm_first_run();
                        }
                    });
                });
            });
    }

    /// 确认首次启动选择：写入游戏目录，重建 paths，持久化，关闭引导。
    fn confirm_first_run(&mut self) {
        let path = self.first_run_path.trim().to_string();
        if path.is_empty() {
            return;
        }
        self.config.game_dir = path;
        self.config.first_run_done = true;
        self.paths = self.config.paths();
        let _ = self.paths.ensure_dirs();
        self.persist_accounts();
        self.first_run = false;
    }

    /// 加入或更新账号（同 UUID 视为已存在，覆盖资料与令牌），并设为当前账号。
    fn upsert_account(&mut self, account: Account) {
        let uuid = account.uuid.clone();
        if let Some(slot) = self.accounts.iter_mut().find(|a| a.uuid == uuid) {
            *slot = account.clone();
        } else {
            self.accounts.push(account.clone());
        }
        self.account = Some(account);
        self.active_uuid = uuid;
        self.persist_accounts();
    }

    /// 切换到指定 UUID 的账号；找不到则清空当前账号。
    fn switch_account(&mut self, uuid: &str) {
        if let Some(a) = self.accounts.iter().find(|a| a.uuid == uuid).cloned() {
            self.account = Some(a);
            self.active_uuid = uuid.to_string();
        } else {
            self.account = None;
            self.active_uuid = String::new();
        }
        self.persist_accounts();
    }

    /// 删除账号；若删的是当前账号，自动切到列表中剩余第一个。
    fn remove_account(&mut self, uuid: &str) {
        self.accounts.retain(|a| a.uuid != uuid);
        if self.active_uuid == uuid {
            let next = self.accounts.first().cloned();
            self.account = next;
            self.active_uuid = self
                .account
                .as_ref()
                .map(|a| a.uuid.clone())
                .unwrap_or_default();
        }
        self.persist_accounts();
    }

    /// 加载加载器页的版本列表。
    fn load_loader_versions(&mut self, ctx: &Context) {
        let kind = self.loader_page.kind;
        let mc = self.loader_page.mc_version.trim().to_string();
        let key = format!("{:?}:{mc}", kind);
        if self.loader_page.loaded_key == key {
            return;
        }
        self.loader_page.loaded_key = key;
        self.loader_page.selected = None;

        let tx = self.tx.clone();
        let client = self.client.clone();
        let ctx = ctx.clone();
        self._rt.spawn(async move {
            match kind {
                LoaderKind::Fabric => match modloaders::list_fabric(&client, &mc).await {
                    Ok(v) => {
                        let _ = tx.send(GuiEvent::LoaderVersions(kind, v));
                    }
                    Err(e) => {
                        let _ = tx.send(GuiEvent::ActionError(format!("{e:#}")));
                    }
                },
                LoaderKind::Quilt => match modloaders::list_quilt(&client, &mc).await {
                    Ok(v) => {
                        let _ = tx.send(GuiEvent::LoaderVersions(kind, v));
                    }
                    Err(e) => {
                        let _ = tx.send(GuiEvent::ActionError(format!("{e:#}")));
                    }
                },
                LoaderKind::Forge => match modloaders::list_forge(&client).await {
                    Ok(all) => {
                        let filtered = modloaders::filter_forge(&all, &mc);
                        let v = filtered
                            .iter()
                            .map(|s| LoaderVersion {
                                version: s.clone(),
                                stable: true,
                            })
                            .collect();
                        let _ = tx.send(GuiEvent::LoaderVersions(kind, v));
                    }
                    Err(e) => {
                        let _ = tx.send(GuiEvent::ActionError(format!("{e:#}")));
                    }
                },
                LoaderKind::NeoForge => match modloaders::list_neoforge(&client).await {
                    Ok(all) => {
                        // NeoForge 版本与 MC 对应：用提示版本过滤
                        let v = all
                            .iter()
                            .filter(|s| {
                                modloaders::neoforge_mc_hint(s) == mc || mc.is_empty()
                            })
                            .map(|s| LoaderVersion {
                                version: s.clone(),
                                stable: true,
                            })
                            .collect();
                        let _ = tx.send(GuiEvent::LoaderVersions(kind, v));
                    }
                    Err(e) => {
                        let _ = tx.send(GuiEvent::ActionError(format!("{e:#}")));
                    }
                },
                LoaderKind::OptiFine => match modloaders::list_optifine(&client, &mc).await {
                    Ok(v) => {
                        let _ = tx.send(GuiEvent::OptiFineVersions(v));
                    }
                    Err(e) => {
                        let _ = tx.send(GuiEvent::ActionError(format!("{e:#}")));
                    }
                },
            }
            ctx.request_repaint();
        });
    }

    /// 开始安装加载器。
    fn start_install_loader(&mut self, ctx: &Context) {
        let kind = self.loader_page.kind;
        let mc = self.loader_page.mc_version.trim().to_string();

        // 在 spawn 前取出选中的版本，避免跨线程借用页面状态。
        let selected_payload: LoaderPayload = match kind {
            LoaderKind::OptiFine => {
                let Some(idx) = self.loader_page.selected else {
                    self.notice = "请先选择一个版本".to_string();
                    self.overlay = Overlay::Notice;
                    return;
                };
                let Some(entry) = self.loader_page.optifine.get(idx).cloned() else {
                    return;
                };
                LoaderPayload::OptiFine(entry)
            }
            _ => {
                let Some(idx) = self.loader_page.selected else {
                    self.notice = "请先选择一个版本".to_string();
                    self.overlay = Overlay::Notice;
                    return;
                };
                let Some(v) = self.loader_page.versions.get(idx).cloned() else {
                    return;
                };
                match kind {
                    LoaderKind::Fabric => LoaderPayload::Fabric(mc, v.version),
                    LoaderKind::Quilt => LoaderPayload::Quilt(mc, v.version),
                    LoaderKind::Forge => LoaderPayload::Forge(v.version),
                    LoaderKind::NeoForge => LoaderPayload::NeoForge(v.version),
                    LoaderKind::OptiFine => unreachable!(),
                }
            }
        };

        let client = self.client.clone();
        let paths = self.paths.clone();
        let threads = self.config.effective_threads();
        let java_override = self.config.custom_java.trim().to_string();
        let tx_gui = self.tx.clone();
        let ctx = ctx.clone();

        let (tx, mut rx) = mpsc::unbounded_channel::<modloaders::InstallMessage>();
        let fwd = tx_gui.clone();
        let fwd_ctx = ctx.clone();
        self._rt.spawn(async move {
            while let Some(m) = rx.recv().await {
                let _ = fwd.send(GuiEvent::Install(m));
                fwd_ctx.request_repaint();
            }
        });

        self.busy = true;
        self._rt.spawn(async move {
            let java_path = if java_override.is_empty() {
                None
            } else {
                Some(PathBuf::from(java_override))
            };
            let result = match selected_payload {
                LoaderPayload::Fabric(mc, loader) => {
                    modloaders::install_fabric(&client, &paths, &mc, &loader, &tx)
                        .await
                        .map(|_| format!("Fabric {loader} 安装完成"))
                }
                LoaderPayload::Quilt(mc, loader) => {
                    modloaders::install_quilt(&client, &paths, &mc, &loader, &tx)
                        .await
                        .map(|_| format!("Quilt {loader} 安装完成"))
                }
                LoaderPayload::Forge(v) => {
                    modloaders::install_forge(
                        &client, &paths, &v, threads, java_path.as_deref(), &tx,
                    )
                    .await
                    .map(|_| format!("Forge {v} 安装完成"))
                }
                LoaderPayload::NeoForge(v) => {
                    modloaders::install_neoforge(
                        &client, &paths, &v, threads, java_path.as_deref(), &tx,
                    )
                    .await
                    .map(|_| format!("NeoForge {v} 安装完成"))
                }
                LoaderPayload::OptiFine(entry) => {
                    modloaders::install_optifine(
                        &client, &paths, &entry, threads, java_path.as_deref(), &tx,
                    )
                    .await
                    .map(|_| format!("OptiFine {} 安装完成", entry.filename))
                }
            };
            match result {
                Ok(msg) => {
                    let _ = tx_gui.send(GuiEvent::ActionDone(msg));
                }
                Err(e) => {
                    let _ = tx_gui.send(GuiEvent::ActionError(format!("{e:#}")));
                }
            }
            ctx.request_repaint();
        });
    }

    fn start_launch(&mut self, ctx: &Context) {
        if self.account.is_none() {
            self.notice = "请先在右侧选择或添加一个账号（离线 / 微软 / 第三方）".to_string();
            self.overlay = Overlay::Notice;
            return;
        }
        let Some(idx) = self.selected else {
            self.notice = "请先在左侧选择一个版本".to_string();
            self.overlay = Overlay::Notice;
            return;
        };
        let entry = self.shown[idx].clone();

        // 外部启动器目录的版本用外部 paths 读取版本 JSON。
        let paths = self.paths_for_selected();

        // 已安装的本地版本不需要在线清单；仅未安装版本才要求清单已加载。
        let manifest = self.manifest.clone();
        if !paths.version_json(&entry.id).exists() && manifest.is_none() {
            self.notice = "在线版本清单尚未加载，无法下载未安装版本".to_string();
            self.overlay = Overlay::Notice;
            return;
        }

        let tx = self.tx.clone();
        let client = self.client.clone();
        let env = self.env.clone();
        let threads = self.config.effective_threads();
        let custom_java = self.config.custom_java.trim().to_string();
        let ctx = ctx.clone();

        let (dl_tx, dl_rx) = mpsc::unbounded_channel::<DownloadEvent>();
        let fwd_tx = tx.clone();
        let fwd_ctx = ctx.clone();
        self._rt.spawn(async move {
            let mut dl_rx = dl_rx;
            while let Some(e) = dl_rx.recv().await {
                let _ = fwd_tx.send(GuiEvent::Download(e));
                fwd_ctx.request_repaint();
            }
        });

        self.busy = true;
        self._rt.spawn(async move {
            let _ = tx.send(GuiEvent::Status("正在解析版本…".to_string()));
            let detail = match manifest::load_resolved_version(
                &entry.id,
                &paths,
                &client,
                manifest.as_ref(),
            )
            .await
            {
                Ok(d) => d,
                Err(e) => {
                    let _ = tx.send(GuiEvent::LaunchError(format!("{e:#}")));
                    ctx.request_repaint();
                    return;
                }
            };

            let _ = tx.send(GuiEvent::Status("正在收集文件清单…".to_string()));
            let (tasks, _) =
                match download::collect_tasks(&client, &paths, &detail, &env).await {
                    Ok(t) => t,
                    Err(e) => {
                        let _ = tx.send(GuiEvent::LaunchError(format!("{e:#}")));
                        ctx.request_repaint();
                        return;
                    }
                };

            if let Err(e) = download::run(client.clone(), tasks, dl_tx, threads).await {
                let _ = tx.send(GuiEvent::LaunchError(format!("{e:#}")));
                ctx.request_repaint();
                return;
            }

            let _ = tx.send(GuiEvent::Status("正在解压原生库…".to_string()));
            if let Err(e) = download::extract_natives(&paths, &detail, &env) {
                let _ = tx.send(GuiEvent::LaunchError(format!("{e:#}")));
                ctx.request_repaint();
                return;
            }

            // 选择 Java：自定义优先，否则自动检测兼容版本
            let required = java::required_major(&detail);
            let picked = if !custom_java.is_empty() {
                match java::probe(Path::new(&custom_java)) {
                    Ok(j) => j,
                    Err(e) => {
                        let _ = tx.send(GuiEvent::LaunchError(format!(
                            "自定义 Java 不可用：{e:#}"
                        )));
                        ctx.request_repaint();
                        return;
                    }
                }
            } else {
                let available = java::detect_all();
                let Some(p) = java::pick_compatible(&available, required) else {
                    let _ = tx.send(GuiEvent::LaunchError(format!(
                        "未找到 Java（该版本需要 Java {required}），请安装后重试"
                    )));
                    ctx.request_repaint();
                    return;
                };
                p
            };

            let _ = tx.send(GuiEvent::ReadyToLaunch(Box::new(detail), picked.path));
            ctx.request_repaint();
        });
    }

    fn launch_game(&mut self, detail: VersionDetail, java_path: PathBuf, ctx: &Context) {
        // 外部启动器目录的版本用外部目录作为 paths，直接读取它的 libraries/assets/versions。
        let paths = self.paths_for_selected();
        let account = self.account.clone().unwrap();
        let env = self.env.clone();
        let cfg = self.config.clone();
        let tx = self.tx.clone();
        let ctx = ctx.clone();

        self.game_running = true;
        self._rt.spawn(async move {
            // 低配模式：自动按系统内存建议分配 + 附加 GC 调优和锁帧参数
            let (min_mem, max_mem) = if cfg.low_spec_mode {
                let total = rustmc_launcher::core::auto::total_memory_mb().unwrap_or(4096);
                rustmc_launcher::core::auto::suggest_memory(total)
            } else {
                (cfg.min_memory_mb, cfg.max_memory_mb)
            };
            let mut jvm_args = cfg.extra_jvm_args_list();
            let mut game_args = cfg.extra_game_args_list();
            if cfg.low_spec_mode {
                jvm_args.extend(rustmc_launcher::core::auto::low_spec_jvm_args());
                game_args.extend(rustmc_launcher::core::auto::low_spec_game_args());
            }
            let opts = launch::LaunchOptions {
                java: java_path,
                min_memory_mb: min_mem,
                max_memory_mb: max_mem,
                extra_jvm_args: jvm_args,
                extra_game_args: game_args,
                window_width: cfg.window_width,
                window_height: cfg.window_height,
                fullscreen: cfg.fullscreen,
                version_isolation: cfg.version_isolation,
            };
            let result = launch::launch(&paths, &detail, &account, &env, &opts).await;
            let _ = tx.send(GuiEvent::GameExited(
                result.map_err(|e| format!("{e:#}")),
            ));
            ctx.request_repaint();
        });
        self.busy = false;
        self.status = "游戏启动中…".to_string();
    }

    /* ----------------------------- 事件处理 ----------------------------- */

    fn handle_event(&mut self, ev: GuiEvent, ctx: &Context) {
        match ev {
            GuiEvent::ManifestLoaded(m) => {
                self.manifest = Some(m);
                self.refresh_shown();
            }
            GuiEvent::ManifestError(e) => {
                self.notice = format!("加载版本清单失败：{e}");
                self.overlay = Overlay::Notice;
            }
            GuiEvent::Status(s) => {
                if !s.is_empty() {
                    self.status = s;
                }
            }
            GuiEvent::Download(e) => self.handle_download(e),
            GuiEvent::Install(m) => self.handle_install(m, ctx),
            GuiEvent::DeviceCode(d) => {
                self.device = Some(d);
                self.status = "等待浏览器授权".to_string();
            }
            GuiEvent::Authed(a) => {
                // 登录成功：加入/更新账号列表并持久化，设为当前账号。
                self.upsert_account(a);
                self.login_task = None;
                self.device = None;
                self.overlay = Overlay::None;
            }
            GuiEvent::AuthError(e) => {
                self.login_task = None;
                self.device = None;
                self.notice = format!("登录失败：{e}");
                self.overlay = Overlay::Notice;
            }
            GuiEvent::ReadyToLaunch(detail, java) => {
                // 版本文件已下载完成，刷新本地已安装列表，使其进入「已安装」分组。
                self.refresh_shown();
                self.launch_game(*detail, java, ctx);
            }
            GuiEvent::LaunchError(e) => {
                self.busy = false;
                self.notice = format!("启动失败：{e}");
                self.overlay = Overlay::Notice;
            }
            GuiEvent::GameExited(res) => {
                self.game_running = false;
                match res {
                    Ok(()) => self.status = "游戏已正常退出".to_string(),
                    Err(e) => {
                        self.notice = format!("游戏异常退出：{e}");
                        self.overlay = Overlay::Notice;
                    }
                }
            }
            GuiEvent::LoaderVersions(kind, v) => {
                if self.loader_page.kind == kind {
                    self.loader_page.versions = v;
                    self.loader_page.selected = None;
                }
            }
            GuiEvent::OptiFineVersions(v) => {
                self.loader_page.optifine = v;
                self.loader_page.selected = None;
            }
            GuiEvent::SearchResults(r) => {
                self.content_page.results = r.hits;
                self.content_page.total = r.total_hits;
            }
            GuiEvent::ProjectVersions(v) => {
                self.content_page.versions = v;
                self.content_page.selected_version = None;
            }
            GuiEvent::ActionDone(s) => {
                self.busy = false;
                self.status = s;
                self.refresh_installed();
                self.refresh_shown();
            }
            GuiEvent::ActionError(e) => {
                self.busy = false;
                self.notice = e;
                self.overlay = Overlay::Notice;
            }
            GuiEvent::Ai(ev) => match ev {
                AiUiEvent::Line(line) => {
                    self.chat_lines.push(line);
                    ctx.request_repaint();
                }
                AiUiEvent::Done => {
                    self.ai_busy = false;
                    // AI 干完活顺手刷新一下版本/模组列表
                    self.refresh_shown();
                    ctx.request_repaint();
                }
            },
        }
    }

    fn handle_download(&mut self, e: DownloadEvent) {
        match e {
            DownloadEvent::Start {
                total_files,
                total_bytes,
            } => {
                self.progress = Progress {
                    total_files,
                    total_bytes,
                    ..Default::default()
                };
            }
            DownloadEvent::FileDone {
                completed,
                total,
                finished_bytes,
                total_bytes,
                name,
            } => {
                self.progress.completed_files = completed;
                self.progress.total_files = total;
                self.progress.done_bytes = finished_bytes;
                self.progress.total_bytes = total_bytes;
                self.progress.current = name;
            }
            DownloadEvent::Message(m) => self.status = m,
            DownloadEvent::Finish => {}
        }
    }

    fn handle_install(&mut self, m: modloaders::InstallMessage, _ctx: &Context) {
        match m {
            modloaders::InstallMessage::Status(s) => self.status = s,
            modloaders::InstallMessage::Download(e) => self.handle_download(e),
        }
    }

    fn refresh_shown(&mut self) {
        if self.filter == 3 {
            // 「已安装」视图：只列出本地 versions 目录中已登记的版本。
            self.shown = self
                .local_installed_ids()
                .into_iter()
                .map(|id| {
                    let vtype = self.local_version_type(&id);
                    VersionEntry {
                        id,
                        version_type: vtype,
                        url: String::new(),
                        sha1: String::new(),
                        release_time: String::new(),
                    }
                })
                .collect();
        } else {
            match &self.manifest {
                Some(m) => {
                    self.shown = m
                        .versions
                        .iter()
                        .filter(|v| match self.filter {
                            1 => v.version_type == "release",
                            2 => v.version_type == "snapshot",
                            _ => true,
                        })
                        .cloned()
                        .collect();
                    // 合并本地版本（加载器安装的版本不在官方清单中）
                    self.merge_local_versions();
                }
                None => {
                    // 清单尚未加载或离线：退化为仅展示本地版本。
                    self.shown = Vec::new();
                    self.merge_local_versions();
                }
            }
        }

        // 已安装版本排在前面，便于直接选择启动；其余按名称排序。
        let installed: Vec<String> = self.local_installed_ids();
        let installed_set: std::collections::HashSet<&str> =
            installed.iter().map(|s| s.as_str()).collect();
        self.shown
            .sort_by_key(|v| (!installed_set.contains(v.id.as_str()), v.id.clone()));

        // 选中索引越界时回退到第一项。
        if self.selected.is_none() || self.selected >= Some(self.shown.len()) {
            self.selected = if self.shown.is_empty() { None } else { Some(0) };
        }
    }

    /// 扫描本启动器目录 + 外部挂载目录的 `versions/`，返回所有含 `<id>.json` 的版本 id（排序）。
    /// 同时把外部版本的来源目录写入 `version_origins`。
    fn local_installed_ids(&mut self) -> Vec<String> {
        self.version_origins.clear();
        let mut scan_dirs: Vec<std::path::PathBuf> = vec![self.paths.base_dir.clone()];
        for s in &self.config.external_dirs {
            let p = std::path::PathBuf::from(s);
            if p.join("versions").is_dir() && !scan_dirs.contains(&p) {
                scan_dirs.push(p);
            }
        }

        let mut ids: Vec<String> = Vec::new();
        for base in &scan_dirs {
            let versions_dir = base.join("versions");
            let Ok(entries) = std::fs::read_dir(&versions_dir) else {
                continue;
            };
            for e in entries.flatten() {
                if !e.path().is_dir() {
                    continue;
                }
                let Some(name) = e.file_name().to_str().map(|s| s.to_string()) else {
                    continue;
                };
                if e.path().join(format!("{name}.json")).exists() && !ids.contains(&name) {
                    ids.push(name.clone());
                    if *base != self.paths.base_dir {
                        self.version_origins.insert(name, base.clone());
                    }
                }
            }
        }
        ids.sort();
        ids
    }

    /// 本地是否已登记该版本（存在版本 JSON）。
    fn is_installed(&self, id: &str) -> bool {
        self.paths.version_json(id).exists()
    }

    /// 本地是否存在客户端 jar（已完整下载的原版标志）。
    fn has_local_jar(&self, id: &str) -> bool {
        self.paths.version_jar(id).exists()
    }

    /// 读取本地版本 JSON 中的 `type` 字段；读不到按本地自定义版本处理。
    fn local_version_type(&self, id: &str) -> String {
        local_version_type_at(&self.paths.version_json(id))
    }

    /// 按版本 id 判断所属加载器（用于详情展示）。
    fn loader_label(id: &str) -> Option<&'static str> {
        let l = id.to_lowercase();
        if l.contains("neoforge") {
            Some("NeoForge")
        } else if l.contains("forge") {
            Some("Forge")
        } else if l.contains("fabric") {
            Some("Fabric")
        } else if l.contains("quilt") {
            Some("Quilt")
        } else if l.contains("optifine") {
            Some("OptiFine")
        } else {
            None
        }
    }

    /// 扫描 versions 目录（本启动器目录 + 外部挂载目录），把本地已有但不在官方清单中的版本加入列表。
    fn merge_local_versions(&mut self) {
        self.version_origins.clear();

        // 收集所有要扫描的 (base_dir, is_external)
        let mut scan_dirs: Vec<std::path::PathBuf> = vec![self.paths.base_dir.clone()];
        for s in &self.config.external_dirs {
            let p = std::path::PathBuf::from(s);
            if p.join("versions").is_dir() && !scan_dirs.contains(&p) {
                scan_dirs.push(p);
            }
        }

        for base in &scan_dirs {
            let versions_dir = base.join("versions");
            let Ok(entries) = std::fs::read_dir(&versions_dir) else {
                continue;
            };
            for e in entries.flatten() {
                if !e.path().is_dir() {
                    continue;
                }
                let Some(name) = e.file_name().to_str().map(|s| s.to_string()) else {
                    continue;
                };
                let json = e.path().join(format!("{name}.json"));
                if !json.exists() {
                    continue;
                }
                // 同名版本优先用第一个扫描到的（本启动器目录优先）
                let exists = self.shown.iter().any(|v| v.id == name);
                if !exists {
                    let vtype = local_version_type_at(&json);
                    self.shown.push(VersionEntry {
                        id: name.clone(),
                        version_type: vtype,
                        url: String::new(),
                        sha1: String::new(),
                        release_time: String::new(),
                    });
                    // 记录外部来源（本启动器目录不记录，启动时用默认 paths）
                    if *base != self.paths.base_dir {
                        self.version_origins.insert(name, base.clone());
                    }
                }
            }
        }
    }

    /// 当前选中版本对应的 LauncherPaths（外部目录版本用外部 paths）。
    fn paths_for_selected(&self) -> LauncherPaths {
        if let Some(idx) = self.selected {
            if let Some(entry) = self.shown.get(idx) {
                if let Some(orig) = self.version_origins.get(&entry.id) {
                    return LauncherPaths::new(orig.clone());
                }
            }
        }
        self.paths.clone()
    }

    /// 用系统文件管理器打开游戏目录（不引入额外依赖，按平台调用系统命令）。
    fn open_dir_in_explorer(&self) {
        let dir = self.paths.base_dir.clone();
        #[cfg(target_os = "linux")]
        {
            let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
        }
        #[cfg(target_os = "windows")]
        {
            let _ = std::process::Command::new("explorer.exe").arg(dir).spawn();
        }
        #[cfg(target_os = "macos")]
        {
            let _ = std::process::Command::new("open").arg(dir).spawn();
        }
    }

    fn refresh_installed(&mut self) {
        let game_dir = self.active_game_dir();
        let paths = self.paths_for_selected();
        self.content_page.installed =
            content::list_installed(&paths, &game_dir, self.content_page.kind);
    }
}

/// 读取版本 JSON 文件中的 `type` 字段；读不到按本地自定义版本处理。
fn local_version_type_at(json_path: &std::path::Path) -> String {
    let text = match std::fs::read_to_string(json_path) {
        Ok(t) => t,
        Err(_) => return "custom".to_string(),
    };
    let v: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return "custom".to_string(),
    };
    v.get("type")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "custom".to_string())
}

/* ----------------------------- eframe App ----------------------------- */

impl eframe::App for GuiApp {
    fn logic(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        while let Ok(ev) = self.rx.try_recv() {
            self.handle_event(ev, ctx);
        }
        // 接收后台下载完成的图标，上传为 GPU 纹理
        if let Some(rx) = &mut self.icon_rx {
            while let Ok((url, bytes)) = rx.try_recv() {
                self.icon_loading.remove(&url);
                if bytes.is_empty() {
                    self.icon_failed.insert(url, std::time::Instant::now());
                    continue;
                }
                match image::load_from_memory(&bytes) {
                    Ok(img) => {
                        let rgba = img.to_rgba8();
                        let (w, h) = rgba.dimensions();
                        let color_image = egui::ColorImage::from_rgba_unmultiplied(
                            [w as usize, h as usize],
                            rgba.as_raw(),
                        );
                        let handle = ctx.load_texture(&url, color_image, egui::TextureOptions::default());
                        self.icon_failed.remove(&url);
                        self.icon_textures.insert(url, handle);
                    }
                    Err(_) => {
                        self.icon_failed.insert(url, std::time::Instant::now());
                    }
                }
            }
        }
        // 失败的图标每隔 1 秒重试一次
        let now = std::time::Instant::now();
        let mut retry: Vec<String> = Vec::new();
        for (url, last) in &self.icon_failed {
            if now.duration_since(*last) >= std::time::Duration::from_secs(1)
                && !self.icon_loading.contains(url)
            {
                retry.push(url.clone());
            }
        }
        for url in retry {
            self.icon_failed.remove(&url);
            self.spawn_icon_download(&url);
        }
        // 拖拽文件导入
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if !dropped.is_empty() {
            for f in dropped {
                self.handle_dropped_file(f.path().to_path_buf(), ctx);
            }
        }
        if matches!(self.tab, Tab::Content) {
            let game_dir = self.active_game_dir();
            let paths = self.paths_for_selected();
            let expected = content::list_installed(&paths, &game_dir, self.content_page.kind);
            if expected != self.content_page.installed {
                self.content_page.installed = expected;
            }
        }
        ctx.request_repaint_after(Duration::from_millis(500));
    }

    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        Panel::top("top")
            .exact_size(44.0)
            .show(root, |ui| self.ui_top(ui));

        Panel::bottom("bottom")
            .exact_size(if self.busy { 96.0 } else { 58.0 })
            .show(root, |ui| self.ui_bottom(ui));

        // 标签栏（放在顶部下方，用一个顶部面板）
        Panel::top("tabs")
            .exact_size(38.0)
            .show(root, |ui| self.ui_tabs(ui));

        egui::CentralPanel::default().show(root, |ui| match self.overlay {
            Overlay::DeviceLogin => self.ui_device_login(ui),
            Overlay::Notice => self.ui_notice(ui),
            Overlay::None => match self.tab {
                Tab::Play => self.ui_play(ui),
                Tab::Loaders => self.ui_loaders(ui),
                Tab::Content => self.ui_content(ui),
                Tab::Ai => self.ui_ai(ui),
                Tab::Settings => self.ui_settings(ui),
            },
        });

        // 首次启动引导：选择游戏目录（居中模态窗）。
        if self.first_run {
            self.ui_first_run(root.ctx());
        }
    }
}

/* ----------------------------- 顶部 / 标签 / 底部 ----------------------------- */

impl GuiApp {
    fn ui_top(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.heading(
                RichText::new("RustMC Launcher")
                    .color(Color32::from_rgb(125, 207, 255)),
            );
            ui.label(
                RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                    .color(Color32::from_gray(110)),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                match &self.account {
                    Some(a) => {
                        let color = match a.kind {
                            AccountKind::Microsoft => Color32::from_rgb(158, 206, 106),
                            AccountKind::ThirdParty => Color32::from_rgb(125, 207, 255),
                            AccountKind::Offline => Color32::from_rgb(190, 190, 190),
                        };
                        ui.label(
                            RichText::new(format!("● {} （{}）", a.name, a.kind_label()))
                                .color(color),
                        );
                        if ui.button("退出当前").clicked() {
                            // 仅取消选中，账号记录仍保留在本地列表中。
                            self.account = None;
                            self.active_uuid = String::new();
                            self.persist_accounts();
                        }
                    }
                    None => {
                        ui.label(
                            RichText::new("○ 未登录")
                                .color(Color32::from_rgb(224, 175, 104)),
                        );
                    }
                }
            });
        });
    }

    fn ui_tabs(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let tabs = [
                (Tab::Play, "🎮 游戏"),
                (Tab::Loaders, "🧩 加载器"),
                (Tab::Content, "📦 内容"),
                (Tab::Ai, "🤖 AI 助手"),
                (Tab::Settings, "⚙ 设置"),
            ];
            for (t, label) in tabs {
                let selected = self.tab == t;
                let resp = ui.selectable_label(selected, label);
                if resp.clicked() {
                    self.tab = t;
                    if matches!(t, Tab::Content) {
                        self.refresh_installed();
                    }
                }
            }
        });
    }

    fn ui_bottom(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        if self.busy {
            let p = &self.progress;
            let pct = if p.total_files > 0 {
                p.completed_files as f32 / p.total_files as f32
            } else {
                0.0
            };
            let bar = ProgressBar::new(pct).text(format!(
                "文件 {}/{}   {:.1}/{:.1} MB   {}",
                p.completed_files,
                p.total_files,
                p.done_bytes as f64 / 1_048_576.0,
                p.total_bytes as f64 / 1_048_576.0,
                p.current
            ));
            ui.add(bar);
        }

        ui.horizontal(|ui| {
            // 状态文本
            ui.label(
                RichText::new(if self.status.is_empty() {
                    "就绪"
                } else {
                    &self.status
                })
                .color(Color32::from_gray(190)),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let disabled = self.busy || self.game_running;
                let btn = ui.add_enabled(
                    !disabled,
                    egui::Button::new(
                        RichText::new(if self.game_running {
                            "游戏运行中"
                        } else {
                            "▶ 启动游戏"
                        })
                        .size(18.0)
                        .color(Color32::BLACK),
                    )
                    .fill(Color32::from_rgb(125, 207, 255)),
                );
                if btn.clicked() {
                    self.start_launch(ui.ctx());
                }
            });
        });
    }
}

/* ----------------------------- Play 页 ----------------------------- */

impl GuiApp {
    fn ui_play(&mut self, ui: &mut egui::Ui) {
        let installed_color = Color32::from_rgb(86, 200, 130);
        // 顶层取窗口可用高度，固定给版本列表 ScrollArea，避免 group 内部 available_height 算错
        let window_h = ui.available_height();
        ui.horizontal(|ui| {
            // 左侧：版本列表（先用 vertical 约束宽度和高度，保证内部为垂直布局）
            ui.vertical(|ui| {
                ui.set_min_width(290.0);
                ui.set_max_width(290.0);
                ui.set_min_height(window_h);
                ui.group(|ui| {
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        for (f, label) in [(0, "全部"), (1, "正式版"), (2, "快照版"), (3, "已安装")] {
                            let resp = ui.selectable_label(self.filter == f, label);
                            if resp.clicked() {
                                self.filter = f;
                                self.refresh_shown();
                            }
                        }
                    });
                    ui.separator();

                    // 分组：已安装在前，在线未下载在后。
                    let installed_idx: Vec<usize> = self
                        .shown
                        .iter()
                        .enumerate()
                        .filter(|(_, v)| self.is_installed(&v.id))
                        .map(|(i, _)| i)
                        .collect();
                    let missing_idx: Vec<usize> = self
                        .shown
                        .iter()
                        .enumerate()
                        .filter(|(_, v)| !self.is_installed(&v.id))
                        .map(|(i, _)| i)
                        .collect();

                    ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .max_height(ui.available_height() - 90.0)
                        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                        .vscroll(true)
                        .scroll_source(egui::scroll_area::ScrollSource {
                            scroll_bar: true,
                            drag: egui::scroll_area::DragScroll::Always,
                            mouse_wheel: true,
                        })
                        .show(ui, |ui| {
                            // —— 已安装分组 ——
                            if !installed_idx.is_empty() {
                                ui.label(
                                    RichText::new(format!("已安装（{}）", installed_idx.len()))
                                        .strong()
                                        .size(12.0)
                                        .color(installed_color),
                                );
                                for &i in &installed_idx {
                                    let v = &self.shown[i];
                                    let name = match Self::loader_label(&v.id) {
                                        Some(l) => format!("✓  {}  · {l}", v.id),
                                        None => format!("✓  {}", v.id),
                                    };
                                    let resp = ui.selectable_label(
                                        self.selected == Some(i),
                                        RichText::new(name).color(installed_color),
                                    );
                                    let hover = if Self::loader_label(&v.id).is_some() {
                                        "本地已安装版本，可直接启动"
                                    } else if self.has_local_jar(&v.id) {
                                        "本地客户端文件就绪，可直接启动"
                                    } else {
                                        "本地版本，启动时将补齐缺失文件"
                                    };
                                    let clicked = resp.clicked();
                                    resp.on_hover_text(hover);
                                    if clicked {
                                        self.selected = Some(i);
                                    }
                                }
                                if !missing_idx.is_empty() {
                                    ui.add_space(6.0);
                                    ui.separator();
                                }
                            }

                            // —— 在线未安装分组 ——
                            if !missing_idx.is_empty() {
                                ui.label(
                                    RichText::new(format!("在线版本（{}）", missing_idx.len()))
                                        .strong()
                                        .size(12.0)
                                        .color(Color32::from_gray(150)),
                                );
                                for &i in &missing_idx {
                                    let v = &self.shown[i];
                                    let marker = match v.version_type.as_str() {
                                        "release" => "R",
                                        "snapshot" => "S",
                                        _ => "O",
                                    };
                                    let resp = ui.selectable_label(
                                        self.selected == Some(i),
                                        format!("{marker}  {}", v.id),
                                    );
                                    let clicked = resp.clicked();
                                    resp.on_hover_text(format!(
                                        "{}（未下载，启动时在线获取）",
                                        v.version_type
                                    ));
                                    if clicked {
                                        self.selected = Some(i);
                                    }
                                }
                            }

                            if self.shown.is_empty() {
                                ui.add_space(10.0);
                                ui.label(
                                    RichText::new("暂无版本。点击「刷新版本列表」在线获取，或先在加载器页安装。")
                                        .color(Color32::from_gray(150)),
                                );
                            }
                        });
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("⟳ 刷新版本列表").clicked() {
                            self.load_manifest(ui.ctx());
                            self.refresh_shown();
                        }
                        if ui.button("📁 打开游戏目录").clicked() {
                            self.open_dir_in_explorer();
                        }
                    });
                });
            });

            // 右侧：账号 + 版本详情
            ui.vertical(|ui| {
                ui.add_space(4.0);
                ui.group(|ui| {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("账号管理")
                            .strong()
                            .size(16.0)
                            .color(theme::ACCENT),
                    );
                    ui.add_space(4.0);

                    // —— 已保存账号列表（永久保留在本地，点击切换，可删除）——
                    if self.accounts.is_empty() {
                        ui.label(
                            RichText::new("暂无保存的账号，请在下方添加。")
                                .color(Color32::from_gray(130)),
                        );
                    } else {
                        // 账号一般几个就够，不用 ScrollArea（避免和版本列表抢滚轮事件）
                        let snapshot = self.accounts.clone();
                        for a in &snapshot {
                            let active = a.uuid == self.active_uuid;
                            let color = match a.kind {
                                AccountKind::Microsoft => Color32::from_rgb(158, 206, 106),
                                AccountKind::ThirdParty => Color32::from_rgb(125, 207, 255),
                                AccountKind::Offline => Color32::from_rgb(200, 200, 200),
                            };
                            let uuid = a.uuid.clone();
                            let label = format!("{} （{}）", a.name, a.kind_label());
                            let note = a.note.clone();
                            ui.horizontal(|ui| {
                                let resp = ui.selectable_label(
                                    active,
                                    RichText::new(label.clone()).color(color),
                                );
                                if resp.clicked() {
                                    self.switch_account(&uuid);
                                }
                                if !note.is_empty() {
                                    ui.label(
                                        RichText::new(&note)
                                            .color(Color32::from_gray(120))
                                            .small(),
                                    );
                                }
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.button("删除").clicked() {
                                            self.remove_account(&uuid);
                                        }
                                    },
                                );
                            });
                        }
                    }

                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(4.0);

                    // —— 添加离线账号 ——
                    ui.horizontal(|ui| {
                        ui.label("离线:");
                        ui.add_sized(
                            [150.0, 22.0],
                            egui::TextEdit::singleline(&mut self.offline_name)
                                .hint_text("玩家名，如 Steve"),
                        );
                        if ui.button("添加离线账号").clicked() {
                            let name = if self.offline_name.trim().is_empty() {
                                "Player".to_string()
                            } else {
                                self.offline_name.trim().to_string()
                            };
                            self.upsert_account(Account::offline(&name));
                            self.offline_name.clear();
                        }
                    });

                    // —— 添加第三方账号 ——
                    ui.horizontal(|ui| {
                        ui.label("第三方:");
                        ui.add_sized(
                            [110.0, 22.0],
                            egui::TextEdit::singleline(&mut self.add_name)
                                .hint_text("玩家名（必填）"),
                        );
                        ui.add_sized(
                            [110.0, 22.0],
                            egui::TextEdit::singleline(&mut self.add_uuid)
                                .hint_text("UUID（可选）"),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("令牌/备注:");
                        ui.add_sized(
                            [110.0, 22.0],
                            egui::TextEdit::singleline(&mut self.add_token)
                                .hint_text("令牌（可选）"),
                        );
                        ui.add_sized(
                            [110.0, 22.0],
                            egui::TextEdit::singleline(&mut self.add_note)
                                .hint_text("备注（可选）"),
                        );
                        let enabled = !self.add_name.trim().is_empty();
                        if ui
                            .add_enabled(enabled, egui::Button::new("添加第三方"))
                            .clicked()
                        {
                            let acc = Account::third_party(
                                &self.add_name,
                                &self.add_uuid,
                                &self.add_token,
                                &self.add_note,
                            );
                            self.upsert_account(acc);
                            self.add_name.clear();
                            self.add_uuid.clear();
                            self.add_token.clear();
                            self.add_note.clear();
                        }
                    });

                    // —— 微软正版设备码登录 ——
                    ui.horizontal(|ui| {
                        if ui.button("微软账号登录（设备码）").clicked() {
                            self.start_microsoft(ui.ctx());
                        }
                    });
                    ui.add_space(4.0);
                });

                ui.add_space(10.0);
                if let Some(idx) = self.selected
                    && let Some(v) = self.shown.get(idx)
                {
                    let installed = self.is_installed(&v.id);
                    let loader = Self::loader_label(&v.id);
                    ui.group(|ui| {
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new("版本详情")
                                .strong()
                                .size(16.0)
                                .color(theme::ACCENT),
                        );
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label("版本:");
                            ui.label(RichText::new(&v.id).strong());
                        });
                        ui.horizontal(|ui| {
                            ui.label("类型:");
                            ui.label(match loader {
                                Some(l) => l,
                                None if v.version_type == "release" => "正式版",
                                None if v.version_type == "snapshot" => "快照版",
                                None => "本地版本",
                            });
                        });
                        ui.horizontal(|ui| {
                            ui.label("状态:");
                            if installed {
                                let status = if loader.is_some() {
                                    "✓ 已安装，可直接启动"
                                } else if self.has_local_jar(&v.id) {
                                    "✓ 已安装，客户端文件就绪"
                                } else {
                                    "✓ 已登记，启动时补齐缺失文件"
                                };
                                ui.label(RichText::new(status).color(installed_color));
                            } else {
                                ui.label(
                                    RichText::new("○ 未下载，启动时在线获取")
                                        .color(Color32::from_gray(150)),
                                );
                            }
                        });
                        if !v.release_time.is_empty() {
                            ui.horizontal(|ui| {
                                ui.label("发布时间:");
                                ui.label(&v.release_time);
                            });
                        }
                        ui.add_space(4.0);
                    });
                }

                ui.add_space(10.0);
                let root_dir = self.paths.base_dir.display().to_string();
                let isolation_dir: Option<String> = if self.config.version_isolation {
                    self.selected
                        .and_then(|i| self.shown.get(i))
                        .map(|v| self.paths.version_dir(&v.id).display().to_string())
                } else {
                    None
                };
                ui.group(|ui| {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("当前游戏目录")
                            .strong()
                            .size(14.0)
                            .color(theme::ACCENT),
                    );
                    ui.label(
                        RichText::new(root_dir.clone())
                            .color(Color32::from_gray(180))
                            .monospace(),
                    );
                    if let Some(d) = &isolation_dir {
                        ui.add_space(2.0);
                        ui.label(
                            RichText::new("版本隔离已开启，本版本数据目录：")
                                .color(Color32::from_gray(140))
                                .small(),
                        );
                        ui.label(
                            RichText::new(d)
                                .color(Color32::from_rgb(158, 206, 106))
                                .monospace(),
                        );
                    }
                    ui.add_space(2.0);
                });
            });
        });
    }
}

/* ----------------------------- 加载器页 ----------------------------- */

impl GuiApp {
    fn ui_loaders(&mut self, ui: &mut egui::Ui) {
        // 默认 MC 版本：最新正式版
        if self.loader_page.mc_version.is_empty() {
            if let Some(m) = &self.manifest {
                self.loader_page.mc_version = m.latest.release.clone();
            }
        }

        ui.add_space(6.0);
        // 加载器类型
        ui.horizontal(|ui| {
            for kind in LoaderKind::all() {
                let selected = self.loader_page.kind == kind;
                let resp = ui.selectable_label(selected, kind.label());
                if resp.clicked() {
                    self.loader_page.kind = kind;
                    self.loader_page.selected = None;
                }
            }
        });

        ui.add_space(8.0);
        // MC 版本选择
        ui.horizontal(|ui| {
            ui.label("Minecraft 版本:");
            let mut changed = false;
            egui::ComboBox::from_id_salt("mc_version_combo")
                .selected_text(&self.loader_page.mc_version)
                .width(200.0)
                .show_ui(ui, |ui| {
                    ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                        if let Some(m) = &self.manifest {
                            for v in &m.versions {
                                if ui
                                    .selectable_label(
                                        self.loader_page.mc_version == v.id,
                                        &v.id,
                                    )
                                    .clicked()
                                {
                                    self.loader_page.mc_version = v.id.clone();
                                    changed = true;
                                }
                            }
                        }
                    });
                });
            if changed {
                self.loader_page.selected = None;
            }
        });

        ui.add_space(8.0);
        // 版本列表
        let kind = self.loader_page.kind;
        ui.label(RichText::new("可用版本（选择后点击安装）").strong());

        // 底部操作区（按钮 + 说明）需要预留高度，保证按钮始终可见，
        // 不被版本列表挤出窗口。
        let bottom_reserve = 96.0;
        let list_h = (ui.available_height() - bottom_reserve).max(120.0);
        ScrollArea::vertical()
            .max_height(list_h)
            .min_scrolled_height(list_h)
            .show(ui, |ui| {
                match kind {
                    LoaderKind::OptiFine => {
                        for (i, e) in self.loader_page.optifine.iter().enumerate() {
                            let resp = ui.selectable_label(
                                self.loader_page.selected == Some(i),
                                format!("{}  （{}）", e.filename, e.forge),
                            );
                            if resp.clicked() {
                                self.loader_page.selected = Some(i);
                            }
                        }
                    }
                    _ => {
                        for (i, v) in self.loader_page.versions.iter().enumerate() {
                            let text = format!(
                                "{}{}",
                                v.version,
                                if v.stable { "" } else { "  （预览）" }
                            );
                            let resp = ui.selectable_label(
                                self.loader_page.selected == Some(i),
                                text,
                            );
                            if resp.clicked() {
                                self.loader_page.selected = Some(i);
                            }
                        }
                    }
                }
            });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let btn = ui.add_enabled(
                !self.busy,
                egui::Button::new(
                    RichText::new("⬇ 安装所选加载器").size(16.0).color(Color32::BLACK),
                )
                .fill(Color32::from_rgb(125, 207, 255)),
            );
            if btn.clicked() {
                self.start_install_loader(ui.ctx());
            }
            if ui.button("🔄 重新获取版本列表").clicked() {
                self.loader_page.loaded_key = String::new();
            }
        });

        // 触发版本列表加载（有去重）
        let ctx = ui.ctx().clone();
        self.load_loader_versions(&ctx);

        // 说明
        ui.add_space(10.0);
        ui.label(
            RichText::new(
                "说明：Fabric / Quilt 直接生成版本；Forge / NeoForge 会下载官方安装器静默安装；\nOptiFine 会弹出官方安装窗口，请在窗口中点击 Install。",
            )
            .color(Color32::from_gray(150))
            .size(12.0),
        );
    }
}

/* ----------------------------- 内容页：后台动作 ----------------------------- */

impl GuiApp {
    /// 内容搜索。
    fn do_content_search(&mut self, ctx: &Context) {
        let kind = self.content_page.kind;
        let query = self.content_page.query.clone();
        let mc = self.content_mc_filter();
        let loader = self.content_loader_filter();
        let tx = self.tx.clone();
        let client = self.client.clone();
        let ctx = ctx.clone();
        self.content_page.offset = 0;
        self._rt.spawn(async move {
            let mc_f = uses_mc_filter(kind).then_some(mc).flatten();
            let loader_f = uses_loader(kind).then_some(loader).flatten();
            match content::search(
                &client,
                &query,
                kind,
                mc_f.as_deref(),
                loader_f.as_deref(),
                0,
                30,
            )
            .await
            {
                Ok(r) => {
                    let _ = tx.send(GuiEvent::SearchResults(r));
                }
                Err(e) => {
                    let _ = tx.send(GuiEvent::ActionError(format!("{e:#}")));
                }
            }
            ctx.request_repaint();
        });
    }

    /// 加载项目版本。
    fn do_load_project_versions(&mut self, project_id: String, ctx: &Context) {
        let kind = self.content_page.kind;
        let mc = self.content_mc_filter();
        let loader = self.content_loader_filter();
        let tx = self.tx.clone();
        let client = self.client.clone();
        let ctx = ctx.clone();
        self.content_page.selected_project = Some(project_id.clone());
        self._rt.spawn(async move {
            let mc_f = uses_mc_filter(kind).then_some(mc).flatten();
            let loader_f = uses_loader(kind).then_some(loader).flatten();
            match content::project_versions(
                &client,
                &project_id,
                mc_f.as_deref(),
                loader_f.as_deref(),
            )
            .await
            {
                Ok(v) => {
                    let _ = tx.send(GuiEvent::ProjectVersions(v));
                }
                Err(e) => {
                    let _ = tx.send(GuiEvent::ActionError(format!("{e:#}")));
                }
            }
            ctx.request_repaint();
        });
    }

    /// 下载并安装所选内容版本。
    fn do_install_content(&mut self, ctx: &Context) {
        let Some(idx) = self.content_page.selected_version else {
            self.notice = "请先选择一个版本".to_string();
            self.overlay = Overlay::Notice;
            return;
        };
        let Some(version) = self.content_page.versions.get(idx).cloned() else {
            return;
        };
        let kind = self.content_page.kind;
        let client = self.client.clone();
        let paths = self.paths.clone();
        let game_dir = self.active_game_dir();
        let isolated = self.config.version_isolation;
        let threads = self.config.effective_threads();
        let tx_gui = self.tx.clone();
        let ctx = ctx.clone();

        let (tx, mut rx) = mpsc::unbounded_channel::<modloaders::InstallMessage>();
        let fwd = tx_gui.clone();
        let fwd_ctx = ctx.clone();
        self._rt.spawn(async move {
            while let Some(m) = rx.recv().await {
                let _ = fwd.send(GuiEvent::Install(m));
                fwd_ctx.request_repaint();
            }
        });

        self.busy = true;
        self._rt.spawn(async move {
            let result = content::install_version(
                &client, &paths, &game_dir, kind, &version, isolated, threads, &tx,
            )
            .await
            .map(|_| format!("{} 已安装：{}", kind.label(), version.name));
            match result {
                Ok(msg) => {
                    let _ = tx_gui.send(GuiEvent::ActionDone(msg));
                }
                Err(e) => {
                    let _ = tx_gui.send(GuiEvent::ActionError(format!("{e:#}")));
                }
            }
            ctx.request_repaint();
        });
    }

    /// 内容页当前 MC 版本过滤值。
    fn content_mc_filter(&self) -> Option<String> {
        if let Some(idx) = self.selected
            && let Some(v) = self.shown.get(idx)
        {
            // 只有官方版本才用作 MC 过滤
            if v.version_type != "custom" {
                return Some(v.id.clone());
            }
        }
        self.manifest.as_ref().map(|m| m.latest.release.clone())
    }

    /// 内容页当前 loader 过滤值（默认 fabric）。
    fn content_loader_filter(&self) -> Option<String> {
        Some(self.content_page.loader.clone())
    }

    /// 处理拖拽文件。
    fn handle_dropped_file(&mut self, path: PathBuf, ctx: &Context) {
        if !matches!(self.tab, Tab::Content) {
            return;
        }
        let kind = self.content_page.kind;
        if matches!(kind, ContentKind::Modpack) {
            self.install_local_pack(path, ctx);
            return;
        }
        let game_dir = self.active_game_dir();
        match content::import_local(&self.paths, &game_dir, &path, kind) {
            Ok(msg) => {
                self.status = msg;
                self.refresh_installed();
            }
            Err(e) => {
                self.notice = format!("{e:#}");
                self.overlay = Overlay::Notice;
            }
        }
    }

    /// 安装本地整合包。
    fn install_local_pack(&mut self, pack: PathBuf, ctx: &Context) {
        let client = self.client.clone();
        let paths = self.paths.clone();
        let game_dir = self.active_game_dir();
        let isolated = self.config.version_isolation;
        let threads = self.config.effective_threads();
        let tx_gui = self.tx.clone();
        let ctx = ctx.clone();

        let (tx, mut rx) = mpsc::unbounded_channel::<modloaders::InstallMessage>();
        let fwd = tx_gui.clone();
        let fwd_ctx = ctx.clone();
        self._rt.spawn(async move {
            while let Some(m) = rx.recv().await {
                let _ = fwd.send(GuiEvent::Install(m));
                fwd_ctx.request_repaint();
            }
        });

        self.busy = true;
        self._rt.spawn(async move {
            let result = content::install_pack_from_path(
                &client, &paths, &game_dir, &pack, isolated, threads, &tx,
            )
            .await
            .map(|_| "整合包安装完成".to_string());
            match result {
                Ok(msg) => {
                    let _ = tx_gui.send(GuiEvent::ActionDone(msg));
                }
                Err(e) => {
                    let _ = tx_gui.send(GuiEvent::ActionError(format!("{e:#}")));
                }
            }
            ctx.request_repaint();
        });
    }
}

fn uses_mc_filter(kind: ContentKind) -> bool {
    matches!(kind, ContentKind::Mod | ContentKind::World | ContentKind::Modpack)
}
fn uses_loader(kind: ContentKind) -> bool {
    matches!(kind, ContentKind::Mod | ContentKind::Modpack)
}

/* ----------------------------- 内容页 UI ----------------------------- */

impl GuiApp {
    fn ui_content(&mut self, ui: &mut egui::Ui) {
        // 首次进入或切换类型时自动搜索全部内容
        if !self.content_page.auto_loaded {
            self.content_page.auto_loaded = true;
            let ctx = ui.ctx().clone();
            self.do_content_search(&ctx);
        }
        // 整页外层滚动，保证窗口较矮时所有按钮（下载/导入/删除）都能滚到。
        ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(4.0);
                // 内容类型
                ui.horizontal(|ui| {
                    for kind in ContentKind::all() {
                        let resp = ui.selectable_label(
                            self.content_page.kind == kind,
                            kind.label(),
                        );
                        if resp.clicked() {
                            self.content_page.kind = kind;
                            self.content_page.results.clear();
                            self.content_page.versions.clear();
                            self.content_page.selected_project = None;
                            self.content_page.auto_loaded = false;
                            self.refresh_installed();
                        }
                    }
                });

                ui.add_space(6.0);
                // 过滤与搜索
                ui.horizontal(|ui| {
                    let kind = self.content_page.kind;
                    if uses_loader(kind) {
                        ui.label("加载器:");
                        egui::ComboBox::from_id_salt("content_loader_combo")
                            .selected_text(&self.content_page.loader)
                            .show_ui(ui, |ui| {
                                for l in ["fabric", "quilt", "neoforge", "forge"] {
                                    if ui
                                        .selectable_label(
                                            self.content_page.loader == l,
                                            l,
                                        )
                                        .clicked()
                                    {
                                        self.content_page.loader = l.to_string();
                                    }
                                }
                            });
                    }
                    ui.label("搜索:");
                    let resp = ui.add_sized(
                        [240.0, 24.0],
                        egui::TextEdit::singleline(&mut self.content_page.query)
                            .hint_text("输入关键词，回车搜索"),
                    );
                    if resp.lost_focus()
                        && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    {
                        self.do_content_search(ui.ctx());
                    }
                    if ui.button("🔍 搜索").clicked() {
                        self.do_content_search(ui.ctx());
                    }
                });

                ui.add_space(6.0);
                // 左右分栏：搜索结果 + 版本
                let mut clicked_project: Option<String> = None;
                ui.horizontal(|ui| {
                    // 左：搜索结果（vertical 约束宽度）
                    ui.vertical(|ui| {
                        ui.set_min_width(340.0);
                        ui.set_max_width(340.0);
                        ui.group(|ui| {
                            ui.label(RichText::new("搜索结果").strong());
                            if self.content_page.results.is_empty() {
                                ui.label(
                                    RichText::new("（暂无结果，点击搜索）")
                                        .color(Color32::from_gray(130)),
                                );
                            }
                            // 先收集数据避免借用冲突
                            let selected_pid = self.content_page.selected_project.clone();
                            let results: Vec<(String, Option<String>, String, String, u64)> =
                                self.content_page.results.iter().map(|h| {
                                    (
                                        h.project_id.clone(),
                                        h.icon_url.clone(),
                                        h.title.clone(),
                                        h.author.clone(),
                                        h.downloads,
                                    )
                                }).collect();
                            for (pid, icon_url, title, author, dl) in &results {
                                let selected = selected_pid.as_deref() == Some(pid.as_str());
                                ui.horizontal(|ui| {
                                    self.ui_icon(ui, icon_url);
                                    let text = format!(
                                        "{}\n{} · ⬇ {}",
                                        title, author, dl
                                    );
                                    let resp = ui.selectable_label(selected, text);
                                    if resp.clicked() {
                                        clicked_project = Some(pid.clone());
                                    }
                                });
                            }
                        });
                    });

                    // 右：版本（vertical 约束宽度）
                    ui.vertical(|ui| {
                        ui.set_min_width(300.0);
                        ui.set_max_width(300.0);
                        ui.group(|ui| {
                            ui.label(RichText::new("版本（选择后下载）").strong());
                            if self.content_page.versions.is_empty() {
                                ui.label(
                                    RichText::new("（请先在左侧选择项目）")
                                        .color(Color32::from_gray(130)),
                                );
                            }
                            for (i, v) in
                                self.content_page.versions.iter().enumerate()
                            {
                                let text = format!(
                                    "{} [{}]",
                                    v.name,
                                    v.game_versions
                                        .last()
                                        .map(|s| s.as_str())
                                        .unwrap_or("")
                                );
                                let resp = ui.selectable_label(
                                    self.content_page.selected_version == Some(i),
                                    text,
                                );
                                if resp.clicked() {
                                    self.content_page.selected_version = Some(i);
                                }
                            }
                            ui.add_space(4.0);
                            let btn = ui.add_enabled(
                                !self.busy,
                                egui::Button::new(
                                    RichText::new("⬇ 下载安装所选版本")
                                        .color(Color32::BLACK),
                                )
                                .fill(Color32::from_rgb(125, 207, 255)),
                            );
                            if btn.clicked() {
                                self.do_install_content(ui.ctx());
                            }
                        });
                    });
                });
                if let Some(pid) = clicked_project {
                    self.do_load_project_versions(pid, &ui.ctx().clone());
                }

                ui.add_space(8.0);
                // 本地导入
                ui.group(|ui| {
                    ui.add_space(4.0);
                    ui.label(RichText::new("本地导入").strong());
                    ui.horizontal(|ui| {
                        ui.label(
                            "将文件拖拽到窗口，或在下方粘贴完整路径后点击导入：",
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            [420.0, 24.0],
                            egui::TextEdit::singleline(&mut self.content_page.local_path)
                                .hint_text("/path/to/file.jar 或 .zip"),
                        );
                        if ui.button("导入").clicked() {
                            let p = self.content_page.local_path.trim().to_string();
                            if !p.is_empty() {
                                self.handle_dropped_file(
                                    PathBuf::from(p),
                                    ui.ctx(),
                                );
                            }
                        }
                    });
                    ui.add_space(2.0);
                });

                ui.add_space(8.0);
                // 已安装列表
                ui.group(|ui| {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(format!(
                            "已安装{}（{}）",
                            self.content_page.kind.label(),
                            self.content_page.installed.len()
                        ))
                        .strong(),
                    );
                    ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
                        if self.content_page.installed.is_empty() {
                            ui.label(
                                RichText::new("（暂无）").color(Color32::from_gray(130)),
                            );
                        }
                        let names = self.content_page.installed.clone();
                        for name in names {
                            ui.horizontal(|ui| {
                                ui.label(&name);
                                if ui.button("删除").clicked() {
                                    let kind = self.content_page.kind;
                                    let game_dir = self.active_game_dir();
                                    match content::delete_installed(
                                        &self.paths, &game_dir, kind, &name,
                                    ) {
                                        Ok(()) => self.refresh_installed(),
                                        Err(e) => {
                                            self.notice = format!("{e:#}");
                                            self.overlay = Overlay::Notice;
                                        }
                                    }
                                }
                            });
                        }
                    });
                    ui.add_space(2.0);
                });
            });
    }
}

/* ----------------------------- 设置页 UI ----------------------------- */

impl GuiApp {
    fn ui_settings(&mut self, ui: &mut egui::Ui) {
        ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.add_space(6.0);

            // 游戏目录
            ui.group(|ui| {
                ui.add_space(4.0);
                ui.label(RichText::new("游戏目录").strong().size(15.0));
                ui.label(
                    RichText::new(
                        "版本、模组、资源包、存档等所有文件的存放位置（.minecraft）。\n为不同整合包设置独立目录可实现多实例。",
                    )
                    .color(Color32::from_gray(150)).size(12.0),
                );
                ui.add_sized(
                    [520.0, 24.0],
                    egui::TextEdit::singleline(&mut self.config.game_dir)
                        .hint_text("留空使用默认位置"),
                );
                ui.label(
                    RichText::new(format!(
                        "当前生效：{}",
                        self.config.resolved_game_dir().display()
                    ))
                    .monospace()
                    .color(Color32::from_gray(170)),
                );
                ui.add_space(2.0);
            });

            ui.add_space(8.0);

            // 外部启动器目录（直接挂载读取其他启动器的版本与存档）
            ui.group(|ui| {
                ui.add_space(4.0);
                ui.label(RichText::new("外部启动器目录").strong().size(15.0));
                ui.label(
                    RichText::new(
                        "直接挂载其他启动器（HMCL / PCL / 官方 .minecraft）的目录，\n无需复制即可在游戏页看到它们已下载的版本，选中即用原目录启动。",
                    )
                    .color(Color32::from_gray(150)).size(12.0),
                );
                // 列出已挂载目录
                let mut to_remove: Option<usize> = None;
                for (i, dir) in self.config.external_dirs.iter().enumerate() {
                    ui.horizontal(|ui| {
                        let exists = std::path::Path::new(dir).join("versions").is_dir();
                        let mark = if exists { "" } else { " （目录不存在）" };
                        ui.label(
                            RichText::new(format!("• {}{}", dir, mark))
                                .monospace()
                                .color(if exists { Color32::from_gray(190) } else { Color32::from_rgb(224,108,117) })
                                .size(12.0),
                        );
                        if ui.button("移除").clicked() {
                            to_remove = Some(i);
                        }
                    });
                }
                if let Some(i) = to_remove {
                    self.config.external_dirs.remove(i);
                    self.version_origins.clear();
                    let _ = config::save(&self.config);
                }
                // 添加新目录
                ui.horizontal(|ui| {
                    ui.label("添加：");
                    ui.add_sized(
                        [380.0, 24.0],
                        egui::TextEdit::singleline(&mut self.ext_dir_input)
                            .hint_text("/home/user/.minecraft"),
                    );
                    if ui.button("添加目录").clicked() {
                        let d = self.ext_dir_input.trim().to_string();
                        if !d.is_empty()
                            && !self.config.external_dirs.contains(&d)
                        {
                            self.config.external_dirs.push(d);
                            self.ext_dir_input.clear();
                            let _ = config::save(&self.config);
                        }
                    }
                });
                ui.add_space(2.0);
            });

            ui.add_space(8.0);

            // 下载设置
            ui.group(|ui| {
                ui.add_space(4.0);
                ui.label(RichText::new("下载并发").strong().size(15.0));
                ui.add(
                    Slider::new(&mut self.config.download_threads, 0..=256)
                        .text("下载线程数（0 = 自动）"),
                );
                ui.label(
                    RichText::new(format!(
                        "实际生效线程数：{}（范围 0-256，数值越大下载越快，过高可能被限速）",
                        self.config.effective_threads()
                    ))
                    .color(Color32::from_gray(150)).size(12.0),
                );
                ui.add_space(2.0);
            });

            ui.add_space(8.0);

            // 内存
            ui.group(|ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("内存与性能").strong().size(15.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if let Some(total) = rustmc_launcher::core::auto::total_memory_mb() {
                            ui.label(
                                RichText::new(format!("系统总内存：{} MB", total))
                                    .color(Color32::from_gray(140)).small(),
                            );
                        }
                    });
                });
                ui.add(
                    Slider::new(&mut self.config.max_memory_mb, 1024..=32768)
                        .step_by(256.0)
                        .text("最大内存 MB"),
                );
                ui.add(
                    Slider::new(&mut self.config.min_memory_mb, 256..=8192)
                        .step_by(128.0)
                        .text("最小内存 MB"),
                );
                ui.horizontal(|ui| {
                    if ui.button("✨ 自动推荐内存").clicked() {
                        if let Some(total) = rustmc_launcher::core::auto::total_memory_mb() {
                            let (mn, mx) = rustmc_launcher::core::auto::suggest_memory(total);
                            self.config.min_memory_mb = mn;
                            self.config.max_memory_mb = mx;
                        }
                    }
                    ui.checkbox(&mut self.config.low_spec_mode, "🪶 低配机优化");
                });
                ui.label(
                    RichText::new("开启低配优化后：自动按总内存分配、附加 GC 调优、锁 30 帧、降负载。适合 4G 内存以下的老机器。")
                        .color(Color32::from_gray(140)).small(),
                );
                ui.add_space(2.0);
            });

            ui.add_space(8.0);

            // Java
            ui.group(|ui| {
                ui.add_space(4.0);
                ui.label(RichText::new("Java").strong().size(15.0));
                ui.label(
                    RichText::new("自定义 Java 可执行文件路径，留空则自动检测兼容版本。")
                        .color(Color32::from_gray(150)).size(12.0),
                );
                ui.add_sized(
                    [520.0, 24.0],
                    egui::TextEdit::singleline(&mut self.config.custom_java)
                        .hint_text("如 /usr/lib/jvm/java-17-openjdk/bin/java"),
                );
                // 显示检测到的 Java（缓存结果，避免每次渲染都扫描系统）
                if self.cached_javas.is_none() {
                    self.cached_javas = Some(java::detect_all());
                }
                ui.horizontal(|ui| {
                    ui.label("检测到的 Java：");
                    if ui.button("重新检测").clicked() {
                        self.cached_javas = Some(java::detect_all());
                    }
                });
                if let Some(jv) = &self.cached_javas {
                    let mut sorted = jv.clone();
                    sorted.sort_by_key(|j| j.major);
                    for j in &sorted {
                        ui.label(
                            RichText::new(format!("• {}", j.display()))
                                .color(Color32::from_gray(170)).size(12.0),
                        );
                    }
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button("📥 自动下载 Java 17（推荐）").clicked() && !self.busy {
                        self.busy = true;
                        self.status = "正在下载 Temurin JRE 17…".to_string();
                        let client = self.client.clone();
                        let base = self.config.resolved_game_dir();
                        let tx = self.tx.clone();
                        self._rt.spawn(async move {
                            match java::download_jre(&client, &base, 17).await {
                                Ok(p) => {
                                    let _ = tx.send(GuiEvent::ActionDone(format!(
                                        "Java 17 已下载到：{}",
                                        p.display()
                                    )));
                                }
                                Err(e) => {
                                    let _ = tx.send(GuiEvent::ActionError(format!(
                                        "下载 Java 失败：{e:#}"
                                    )));
                                }
                            }
                        });
                    }
                    if ui.button("📥 自动下载 Java 21（新版本）").clicked() && !self.busy {
                        self.busy = true;
                        self.status = "正在下载 Temurin JRE 21…".to_string();
                        let client = self.client.clone();
                        let base = self.config.resolved_game_dir();
                        let tx = self.tx.clone();
                        self._rt.spawn(async move {
                            match java::download_jre(&client, &base, 21).await {
                                Ok(p) => {
                                    let _ = tx.send(GuiEvent::ActionDone(format!(
                                        "Java 21 已下载到：{}",
                                        p.display()
                                    )));
                                }
                                Err(e) => {
                                    let _ = tx.send(GuiEvent::ActionError(format!(
                                        "下载 Java 失败：{e:#}"
                                    )));
                                }
                            }
                        });
                    }
                });
                ui.label(
                    RichText::new("没有检测到 Java 时，点上面按钮自动下载便携版到游戏目录，不用手动装。")
                        .color(Color32::from_gray(140)).small(),
                );
                ui.add_space(2.0);
            });

            ui.add_space(8.0);

            // 启动参数
            ui.group(|ui| {
                ui.add_space(4.0);
                ui.label(RichText::new("自定义启动参数").strong().size(15.0));
                ui.label("额外 JVM 参数（如 -XX:+UseG1GC -Dfile.encoding=UTF-8）:");
                ui.add_sized(
                    [560.0, 60.0],
                    egui::TextEdit::multiline(&mut self.config.extra_jvm_args)
                        .code_editor(),
                );
                ui.label("额外游戏参数（如 --server mc.example.com --port 25565）:");
                ui.add_sized(
                    [560.0, 40.0],
                    egui::TextEdit::multiline(&mut self.config.extra_game_args)
                        .code_editor(),
                );
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label("窗口宽:");
                    ui.add(Slider::new(&mut self.config.window_width, 0..=3840).text(""));
                    ui.label("高:");
                    ui.add(Slider::new(&mut self.config.window_height, 0..=2160).text(""));
                    ui.checkbox(&mut self.config.fullscreen, "全屏启动");
                });
                ui.add_space(2.0);
                ui.checkbox(&mut self.config.version_isolation, "启用版本隔离");
                ui.label(
                    RichText::new(
                        "开启后每个版本使用独立目录（versions/<版本号>），存档、模组、资源包、\
                         光影、配置互不影响；关闭则所有版本共用游戏根目录。",
                    )
                    .color(Color32::from_gray(140))
                    .small(),
                );
                ui.add_space(2.0);
            });

            // ---------------- AI 助手配置 ----------------
            ui.add_space(8.0);
            ui.group(|ui| {
                ui.label(RichText::new("🤖 AI 助手").strong().size(15.0));
                ui.label(
                    RichText::new("兼容 OpenAI 协议的接口（DeepSeek / 豆包 / 通义 / OpenAI 均可）。Key 只存在本地。")
                        .color(Color32::from_gray(140)).small(),
                );
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label("接口地址:");
                    ui.text_edit_singleline(&mut self.config.ai_api_base)
                        .on_hover_text("例如 https://api.deepseek.com 或 https://ark.cn-beijing.volces.com/api/v3");
                });
                ui.horizontal(|ui| {
                    ui.label("API Key:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.config.ai_api_key)
                            .password(true),
                    )
                    .on_hover_text("你的 AI 服务密钥");
                });
                ui.horizontal(|ui| {
                    ui.label("模型名:");
                    ui.text_edit_singleline(&mut self.config.ai_model)
                        .on_hover_text("例如 deepseek-chat、doubao-pro-32k、gpt-4o-mini");
                });
            });

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                let save = egui::Button::new(
                    RichText::new("💾 保存设置").size(16.0).color(Color32::BLACK),
                )
                .fill(Color32::from_rgb(158, 206, 106));
                if ui.add(save).clicked() {
                    self.save_settings(ui.ctx());
                }
                if ui.button("恢复默认设置").clicked() {
                    // 仅重置其它设置，保留已保存的账号列表与选中项。
                    let accounts = self.config.accounts.clone();
                    let active = self.config.active_account.clone();
                    self.config = LauncherConfig::default();
                    self.config.accounts = accounts;
                    self.config.active_account = active;
                }
            });
            ui.add_space(10.0);
        });
    }

    /// 保存设置并应用（游戏目录变化时重建路径）。
    fn save_settings(&mut self, ctx: &Context) {
        // 先把当前账号列表与选中项写回配置，避免覆盖已保存的账号。
        self.config.accounts = self.accounts.clone();
        self.config.active_account = self.active_uuid.clone();
        if let Err(e) = config::save(&self.config) {
            self.notice = format!("保存失败：{e:#}");
            self.overlay = Overlay::Notice;
            return;
        }
        let new_paths = self.config.paths();
        let dir_changed = new_paths.base_dir != self.paths.base_dir;
        self.paths = new_paths;
        let _ = self.paths.ensure_dirs();
        if dir_changed {
            self.selected = None;
            self.shown.clear();
            self.refresh_shown();
        }
        self.status = "设置已保存".to_string();
        ctx.request_repaint();
    }
}

/* ----------------------------- 覆盖层：设备登录 / 通知 ----------------------------- */

impl GuiApp {
    fn ui_device_login(&mut self, ui: &mut egui::Ui) {
        ui.add_space(16.0);
        ui.label(RichText::new("微软账号登录").strong().size(20.0));
        ui.add_space(12.0);

        if let Some(d) = &self.device {
            ui.group(|ui| {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label("1. 用浏览器打开:");
                    ui.hyperlink(&d.verification_uri);
                });
                ui.horizontal(|ui| {
                    ui.label("2. 输入代码:");
                    ui.label(
                        RichText::new(&d.user_code)
                            .strong().size(22.0)
                            .color(Color32::from_rgb(158, 206, 106))
                            .monospace(),
                    );
                });
                ui.add_space(8.0);
            });
            ui.add_space(12.0);
        }

        ui.label(if self.status.is_empty() {
            "正在请求设备码…"
        } else {
            &self.status
        });
        ui.add_space(12.0);

        if ui.button("取消登录").clicked() {
            if let Some(t) = self.login_task.take() {
                t.abort();
            }
            self.device = None;
            self.overlay = Overlay::None;
        }
    }

    fn ui_notice(&mut self, ui: &mut egui::Ui) {
        ui.add_space(20.0);
        ui.colored_label(
            Color32::from_rgb(224, 175, 104),
            RichText::new(&self.notice).size(17.0),
        );
        ui.add_space(14.0);
        if ui.button("确定").clicked() {
            self.overlay = Overlay::None;
        }
    }
}

/* ============================ AI 助手 ============================ */

impl GuiApp {
    /// AI 可用工具的 JSON Schema。
    fn ai_tools() -> Vec<ToolSpec> {
        use serde_json::json;
        vec![
            ToolSpec::new(
                "list_versions",
                "列出本地已安装的 Minecraft 版本，以及在线可下载的版本（正式版/快照）。无需参数。",
                json!({"type":"object","properties":{}}),
            ),
            ToolSpec::new(
                "download_version",
                "下载并安装指定的 Minecraft 版本（只下载文件，不启动游戏）。参数 version 形如 \"1.20.1\"、\"1.21.4\"。",
                json!({
                    "type":"object",
                    "properties":{
                        "version":{"type":"string","description":"Minecraft 版本号，例如 1.20.1"}
                    },
                    "required":["version"]
                }),
            ),
            ToolSpec::new(
                "install_loader",
                "为指定 MC 版本安装模组加载器（Fabric / Quilt / Forge / NeoForge）。Forge/NeoForge 的 loader_version 形如 \"1.20.1-47.2.0\"。",
                json!({
                    "type":"object",
                    "properties":{
                        "loader":{"type":"string","enum":["fabric","quilt","forge","neoforge"]},
                        "mc_version":{"type":"string","description":"目标 Minecraft 版本，如 1.20.1"},
                        "loader_version":{"type":"string","description":"加载器版本（Fabric/Quilt 填如 0.16.5；Forge/NeoForge 填完整版本号；不确定可留空让 AI 自己查）"}
                    },
                    "required":["loader","mc_version"]
                }),
            ),
            ToolSpec::new(
                "search_mods",
                "在 Modrinth 搜索模组/资源包/光影/整合包。返回前几条结果（含项目 id、名称、下载量）。",
                json!({
                    "type":"object",
                    "properties":{
                        "query":{"type":"string","description":"搜索关键词，如 JEI、 Sodium 优化"},
                        "kind":{"type":"string","enum":["mod","resourcepack","shader","modpack"],"description":"内容类型，默认 mod"},
                        "mc_version":{"type":"string","description":"可选：限定 MC 版本，如 1.20.1"},
                        "loader":{"type":"string","enum":["fabric","forge","neoforge","quilt"],"description":"可选：限定加载器"}
                    },
                    "required":["query"]
                }),
            ),
            ToolSpec::new(
                "install_mod",
                "把指定 Modrinth 项目安装到当前游戏目录（自动挑最新兼容版本）。",
                json!({
                    "type":"object",
                    "properties":{
                        "project_id":{"type":"string","description":"Modrinth 项目 id 或 slug（从 search_mods 结果里拿）"},
                        "mc_version":{"type":"string","description":"目标 MC 版本"},
                        "loader":{"type":"string","enum":["fabric","forge","neoforge","quilt"],"description":"加载器类型"}
                    },
                    "required":["project_id","mc_version"]
                }),
            ),
            ToolSpec::new(
                "launch_game",
                "下载（如未下载）并启动指定版本的 Minecraft 游戏。需要已有账号。",
                json!({
                    "type":"object",
                    "properties":{
                        "version":{"type":"string","description":"要启动的版本 id，例如 1.20.1 或 fabric-loader-0.16.5-1.20.1"}
                    },
                    "required":["version"]
                }),
            ),
        ]
    }

    /// 用户点发送：把消息丢进后台 AI 对话循环。
    fn send_ai_message(&mut self, ctx: &Context) {
        let text = self.chat_input.trim().to_string();
        if text.is_empty() || self.ai_busy {
            return;
        }
        if self.config.ai_api_key.trim().is_empty() {
            self.chat_lines.push(ChatLine::Notice(
                "还没填 API Key。请到「⚙ 设置」最下方填入 AI 接口地址、Key 和模型名。".to_string(),
            ));
            return;
        }
        self.chat_input.clear();
        self.ai_busy = true;
        self.chat_lines.push(ChatLine::User(text.clone()));

        // 快照所有需要在后台用的状态
        let client = self.client.clone();
        let paths = self.paths.clone();
        let cfg = self.config.clone();
        let manifest = self.manifest.clone();
        let tx = self.tx.clone();
        let repaint = ctx.clone();
        let game_dir = cfg.resolved_game_dir();
        let env = self.env.clone();

        // 消息历史：从已显示的行重建（第一次对话时带上 system prompt）
        // 为了简单，每次都用 system + 当前用户消息开始（不保留长历史，避免上下文爆炸）
        let mut messages: Vec<AiMessage> = vec![
            AiMessage {
                role: "system".into(),
                content: Some(rustmc_launcher::core::ai::system_prompt().to_string()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            AiMessage {
                role: "user".into(),
                content: Some(text),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
        ];
        let tools = Self::ai_tools();
        let base = cfg.ai_api_base.clone();
        let key = cfg.ai_api_key.clone();
        let model = cfg.ai_model.clone();

        self._rt.spawn(async move {
            // 最多 8 轮工具调用，防止死循环
            for _round in 0..8 {
                let reply = match rustmc_launcher::core::ai::chat(&client, &base, &key, &model, &messages, &tools).await {
                    Ok(m) => m,
                    Err(e) => {
                        let _ = tx.send(GuiEvent::Ai(AiUiEvent::Line(ChatLine::Notice(format!(
                            "AI 请求失败：{e}"
                        )))));
                        let _ = tx.send(GuiEvent::Ai(AiUiEvent::Done));
                        repaint.request_repaint();
                        return;
                    }
                };

                let has_tool_calls = reply.tool_calls.is_some();
                // 把 assistant 消息塞进历史
                messages.push(reply.clone());

                if !has_tool_calls {
                    // 最终回复，显示给用户
                    if let Some(content) = reply.content.clone() {
                        if !content.trim().is_empty() {
                            let _ = tx.send(GuiEvent::Ai(AiUiEvent::Line(ChatLine::Assistant(content))));
                        }
                    }
                    let _ = tx.send(GuiEvent::Ai(AiUiEvent::Done));
                    repaint.request_repaint();
                    return;
                }

                // 执行每个 tool call
                if let Some(calls) = reply.tool_calls {
                    for call in calls {
                        let _ = tx.send(GuiEvent::Ai(AiUiEvent::Line(ChatLine::ToolCall {
                            name: call.function.name.clone(),
                            args: call.function.arguments.clone(),
                        })));
                        repaint.request_repaint();

                        let result = Self::execute_tool(
                            &call.function.name,
                            &call.function.arguments,
                            &client,
                            &paths,
                            &cfg,
                            &manifest,
                            &game_dir,
                            &env,
                            &tx,
                        )
                        .await;

                        let ok = result.is_ok();
                        let summary = result.clone().unwrap_or_else(|e| e);
                        let _ = tx.send(GuiEvent::Ai(AiUiEvent::Line(ChatLine::ToolResult {
                            name: call.function.name.clone(),
                            ok,
                            summary: summary.chars().take(500).collect(),
                        })));
                        repaint.request_repaint();

                        // 把 tool 结果作为 message 加回历史
                        messages.push(AiMessage {
                            role: "tool".into(),
                            content: Some(result.unwrap_or_else(|e| format!("错误：{e}"))),
                            tool_calls: None,
                            tool_call_id: Some(call.id),
                            name: Some(call.function.name),
                        });
                    }
                }
            }
            // 超过最大轮数
            let _ = tx.send(GuiEvent::Ai(AiUiEvent::Line(ChatLine::Notice(
                "AI 连续工具调用次数过多，已停止。".to_string(),
            ))));
            let _ = tx.send(GuiEvent::Ai(AiUiEvent::Done));
            repaint.request_repaint();
        });
    }

    /// 执行单个 AI 工具调用，返回给模型看的字符串结果。
    #[allow(clippy::too_many_arguments)]
    async fn execute_tool(
        name: &str,
        args: &str,
        client: &reqwest::Client,
        paths: &LauncherPaths,
        cfg: &LauncherConfig,
        manifest: &Option<VersionManifest>,
        game_dir: &Path,
        env: &EnvInfo,
        tx: &UnboundedSender<GuiEvent>,
    ) -> Result<String, String> {
        let v: serde_json::Value = serde_json::from_str(args).map_err(|e| format!("参数 JSON 解析失败：{e}"))?;
        let threads = cfg.effective_threads();

        match name {
            "list_versions" => {
                // 本地已安装
                let local: Vec<String> = std::fs::read_dir(paths.versions_dir())
                    .map(|rd| {
                        rd.filter_map(|e| e.ok())
                            .filter(|e| e.path().join("versions.json").exists() || e.path().join(e.file_name()).with_extension("json").exists())
                            .filter_map(|e| e.file_name().into_string().ok())
                            .collect()
                    })
                    .unwrap_or_default();
                // 在线版本前 30 个正式版
                let online: Vec<&str> = manifest
                    .iter()
                    .flat_map(|m| m.versions.iter())
                    .filter(|e| e.version_type == "release")
                    .take(30)
                    .map(|e| e.id.as_str())
                    .collect();
                Ok(format!(
                    "已安装版本：{}\n最近正式版（可下载）：{}",
                    if local.is_empty() { "(无)".to_string() } else { local.join(", ") },
                    online.join(", ")
                ))
            }

            "download_version" => {
                let ver = v.get("version").and_then(|x| x.as_str()).ok_or("缺少 version 参数")?;
                Self::download_version_to_ready(client, paths, ver, manifest, env, threads, tx).await?;
                Ok(format!("版本 {ver} 已下载就绪，可以启动了。"))
            }

            "install_loader" => {
                let loader = v.get("loader").and_then(|x| x.as_str()).ok_or("缺少 loader")?;
                let mc = v.get("mc_version").and_then(|x| x.as_str()).ok_or("缺少 mc_version")?;
                let loader_ver = v.get("loader_version").and_then(|x| x.as_str()).unwrap_or("");

                let (ltx, lrx) = mpsc::unbounded_channel::<modloaders::InstallMessage>();
                let fwd_tx = tx.clone();
                tokio::spawn(async move {
                    let mut lrx = lrx;
                    while let Some(m) = lrx.recv().await {
                        if let modloaders::InstallMessage::Status(s) = m {
                            let _ = fwd_tx.send(GuiEvent::Status(s));
                        }
                    }
                });

                let id = match loader {
                    "fabric" => {
                        // 没指定 loader 版本就取最新
                        let lv = if loader_ver.is_empty() {
                            modloaders::list_fabric(client, mc).await.map_err(|e| e.to_string())?.first().cloned().ok_or("没有可用的 Fabric 版本")?.version
                        } else {
                            loader_ver.to_string()
                        };
                        modloaders::install_fabric(client, paths, mc, &lv, &ltx).await.map_err(|e| e.to_string())?
                    }
                    "quilt" => {
                        let lv = if loader_ver.is_empty() {
                            modloaders::list_quilt(client, mc).await.map_err(|e| e.to_string())?.first().cloned().ok_or("没有可用的 Quilt 版本")?.version
                        } else {
                            loader_ver.to_string()
                        };
                        modloaders::install_quilt(client, paths, mc, &lv, &ltx).await.map_err(|e| e.to_string())?
                    }
                    "forge" => {
                        modloaders::install_forge(client, paths, loader_ver, threads, None, &ltx).await.map_err(|e| e.to_string())?
                    }
                    "neoforge" => {
                        modloaders::install_neoforge(client, paths, loader_ver, threads, None, &ltx).await.map_err(|e| e.to_string())?
                    }
                    _ => return Err(format!("未知加载器：{loader}")),
                };
                Ok(format!("加载器已安装，新的版本 id 是 {id}。"))
            }

            "search_mods" => {
                let q = v.get("query").and_then(|x| x.as_str()).unwrap_or("");
                let kind_s = v.get("kind").and_then(|x| x.as_str()).unwrap_or("mod");
                let kind = match kind_s {
                    "resourcepack" => ContentKind::ResourcePack,
                    "shader" => ContentKind::ShaderPack,
                    "modpack" => ContentKind::Modpack,
                    _ => ContentKind::Mod,
                };
                let mc = v.get("mc_version").and_then(|x| x.as_str());
                let loader = v.get("loader").and_then(|x| x.as_str());
                let resp = content::search(client, q, kind, mc, loader, 0, 8)
                    .await
                    .map_err(|e| e.to_string())?;
                let lines: Vec<String> = resp
                    .hits
                    .iter()
                    .map(|h| format!("- [{}] {} (id={}, 下载量={}, 作者={})", h.slug.clone().unwrap_or(h.project_id.clone()), h.title, h.project_id, h.downloads, h.author))
                    .collect();
                Ok(format!("找到 {} 个结果：\n{}", resp.total_hits, lines.join("\n")))
            }

            "install_mod" => {
                let pid = v.get("project_id").and_then(|x| x.as_str()).ok_or("缺少 project_id")?;
                let mc = v.get("mc_version").and_then(|x| x.as_str()).unwrap_or("");
                let loader = v.get("loader").and_then(|x| x.as_str());
                let versions = content::project_versions(client, pid, Some(mc), loader)
                    .await
                    .map_err(|e| e.to_string())?;
                let ver = versions.first().ok_or_else(|| "该项目没有适配此版本的文件".to_string())?.clone();

                let (itx, irx) = mpsc::unbounded_channel::<modloaders::InstallMessage>();
                let fwd_tx = tx.clone();
                tokio::spawn(async move {
                    let mut irx = irx;
                    while let Some(m) = irx.recv().await {
                        if let modloaders::InstallMessage::Status(s) = m {
                            let _ = fwd_tx.send(GuiEvent::Status(s));
                        }
                    }
                });
                content::install_version(client, paths, game_dir, ContentKind::Mod, &ver, cfg.version_isolation, threads, &itx)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(format!("模组 {} 已安装到 mods 目录。", ver.name))
            }

            "launch_game" => {
                let ver = v.get("version").and_then(|x| x.as_str()).ok_or("缺少 version")?;
                Self::download_version_to_ready(client, paths, ver, manifest, env, threads, tx).await?;
                // 下载就绪后，构造 ReadyToLaunch 事件让 UI 真正启动游戏
                let detail = manifest::load_resolved_version(ver, paths, client, manifest.as_ref())
                    .await
                    .map_err(|e| format!("解析版本失败：{e:#}"))?;
                let required = java::required_major(&detail);
                let picked = if !cfg.custom_java.trim().is_empty() {
                    java::probe(Path::new(&cfg.custom_java.trim())).map_err(|e| format!("自定义 Java 不可用：{e:#}"))?
                } else {
                    let available = java::detect_all();
                    java::pick_compatible(&available, required).ok_or_else(|| format!("未找到 Java {required}"))?
                };
                let _ = tx.send(GuiEvent::ReadyToLaunch(Box::new(detail), picked.path));
                Ok("已就绪，正在启动游戏…".to_string())
            }

            other => Err(format!("未知工具：{other}")),
        }
    }

    /// 下载一个版本到"可启动"状态（不真正启动）。AI 工具复用。
    async fn download_version_to_ready(
        client: &reqwest::Client,
        paths: &LauncherPaths,
        ver: &str,
        manifest: &Option<VersionManifest>,
        env: &EnvInfo,
        threads: usize,
        tx: &UnboundedSender<GuiEvent>,
    ) -> Result<(), String> {
        let _ = tx.send(GuiEvent::Status(format!("AI：正在解析版本 {ver}…")));
        let detail = manifest::load_resolved_version(ver, paths, client, manifest.as_ref())
            .await
            .map_err(|e| format!("{e:#}"))?;
        let _ = tx.send(GuiEvent::Status("AI：正在收集文件清单…".to_string()));
        let (tasks, _) = download::collect_tasks(client, paths, &detail, env)
            .await
            .map_err(|e| format!("{e:#}"))?;

        let (dl_tx, dl_rx) = mpsc::unbounded_channel::<DownloadEvent>();
        let fwd = tx.clone();
        tokio::spawn(async move {
            let mut dl_rx = dl_rx;
            while let Some(e) = dl_rx.recv().await {
                let _ = fwd.send(GuiEvent::Download(e));
            }
        });

        download::run(client.clone(), tasks, dl_tx, threads)
            .await
            .map_err(|e| format!("{e:#}"))?;
        download::extract_natives(paths, &detail, env).map_err(|e| format!("{e:#}"))?;
        Ok(())
    }

    /// 渲染 AI 聊天页。
    fn ui_ai(&mut self, ui: &mut egui::Ui) {
        ui.vertical(|ui| {
            ui.label(RichText::new("🤖 AI 助手").size(20.0).strong());
            ui.label(
                RichText::new("用自然语言让 AI 帮你下版本、装加载器、搜模组、启动游戏。")
                    .color(theme::TEXT_DIM),
            );
            ui.add_space(8.0);

            // 消息列表：占满剩余空间，给输入框留 70px
            ScrollArea::vertical()
                .auto_shrink([false, false])
                .max_height(ui.available_height() - 70.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in &self.chat_lines {
                        match line {
                            ChatLine::User(t) => {
                                ui.horizontal(|ui| {
                                    ui.add_space(4.0);
                                    ui.colored_label(theme::BLUE, RichText::new("你:").strong());
                                    ui.label(t);
                                });
                            }
                            ChatLine::Assistant(t) => {
                                ui.horizontal_wrapped(|ui| {
                                    ui.add_space(4.0);
                                    ui.colored_label(theme::ACCENT, RichText::new("AI:").strong());
                                    ui.label(t);
                                });
                            }
                            ChatLine::ToolCall { name, args } => {
                                ui.horizontal_wrapped(|ui| {
                                    ui.add_space(4.0);
                                    ui.colored_label(theme::WARN, RichText::new(format!("🔧 调用 {name}")).small());
                                    ui.label(RichText::new(args).small().color(theme::TEXT_DIM));
                                });
                            }
                            ChatLine::ToolResult { name, ok, summary } => {
                                let color = if *ok { theme::ACCENT } else { theme::ERROR };
                                ui.horizontal_wrapped(|ui| {
                                    ui.add_space(4.0);
                                    ui.colored_label(color, RichText::new(format!("  ↳ {name} {}", if *ok { "✓" } else { "✗" })).small());
                                    ui.label(RichText::new(summary).small().color(theme::TEXT_DIM));
                                });
                            }
                            ChatLine::Notice(t) => {
                                ui.horizontal(|ui| {
                                    ui.add_space(4.0);
                                    ui.colored_label(theme::WARN, RichText::new(t).small());
                                });
                            }
                        }
                        ui.add_space(4.0);
                    }
                    if self.ai_busy {
                        ui.label(RichText::new("AI 正在思考…").italics().color(theme::TEXT_DIM));
                    }
                });

            ui.separator();

            // 输入框
            ui.horizontal(|ui| {
                let resp = ui.add(
                    egui::TextEdit::multiline(&mut self.chat_input)
                        .desired_rows(1)
                        .desired_width(ui.available_width() - 100.0)
                        .hint_text("说点什么，比如：帮我装一个 1.20.1 的 Fabric，再装个 JEI 模组"),
                );
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift) {
                    let text = self.chat_input.trim().to_string();
                    if !text.is_empty() {
                        self.send_ai_message(ui.ctx());
                    }
                }
                let btn = ui.add_enabled(!self.ai_busy, egui::Button::new("发送 ⏎"));
                if btn.clicked() {
                    self.send_ai_message(ui.ctx());
                }
            });
        });
    }
}
