//! 终端界面（ratatui）：版本列表、账号登录、下载进度、游戏启动。

use std::io::{self, stdout};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, Clear as TermClear, ClearType,
    EnterAlternateScreen, LeaveAlternateScreen,
};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{
    Alignment, Constraint, Direction, Layout, Rect,
};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap,
};
use ratatui::{Frame, Terminal};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use rustmc_launcher::core::auth::{self, Account};
use rustmc_launcher::core::config::{self, LauncherConfig};
use rustmc_launcher::core::download::DownloadEvent;
use rustmc_launcher::core::java;
use rustmc_launcher::core::launch;
use rustmc_launcher::core::manifest::{
    self, VersionDetail, VersionEntry, VersionManifest,
};
use rustmc_launcher::core::paths::LauncherPaths;
use rustmc_launcher::core::rules::EnvInfo;

const SPINNER: [&str; 10] = [
    "⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏",
];

fn spinner(i: usize) -> &'static str {
    SPINNER[i % SPINNER.len()]
}

/* --------------------------- 事件与状态 --------------------------- */

enum AppEvent {
    ManifestLoaded(VersionManifest),
    ManifestError(String),
    Status(String),
    Download(DownloadEvent),
    DeviceCode(auth::DeviceCode),
    Authed(Account),
    AuthError(String),
    ReadyToLaunch(Box<VersionDetail>, PathBuf),
    LaunchError(String),
}

#[derive(PartialEq, Eq)]
enum Screen {
    Loading,
    List,
    OfflineInput,
    DeviceLogin,
    Working,
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

pub struct App {
    screen: Screen,
    manifest: Option<VersionManifest>,
    shown: Vec<VersionEntry>,
    selected: usize,
    /// 0 全部 / 1 正式版 / 2 快照版
    filter: u8,
    account: Option<Account>,
    config: LauncherConfig,
    accounts: Vec<Account>,
    input: String,
    status: String,
    device: Option<auth::DeviceCode>,
    progress: Progress,
    notice: String,
    spinner: usize,
    login_task: Option<tokio::task::JoinHandle<()>>,
    pending_launch: Option<(VersionDetail, PathBuf)>,
    rx: UnboundedReceiver<AppEvent>,
    tx: UnboundedSender<AppEvent>,
    paths: LauncherPaths,
    client: reqwest::Client,
    env: EnvInfo,
}

impl App {
    pub fn new(paths: LauncherPaths, client: reqwest::Client, env: EnvInfo) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let config = config::load();
        let accounts = config.accounts.clone();
        let account = config.active().cloned();
        let app = App {
            screen: Screen::Loading,
            manifest: None,
            shown: Vec::new(),
            selected: 0,
            filter: 0,
            account,
            config,
            accounts,
            input: String::new(),
            status: String::new(),
            device: None,
            progress: Progress::default(),
            notice: String::new(),
            spinner: 0,
            login_task: None,
            pending_launch: None,
            rx,
            tx,
            paths,
            client,
            env,
        };
        app.reload_manifest();
        app
    }

    /// 把账号列表与当前账号写回配置并持久化到本地。
    fn persist_accounts(&mut self) {
        self.config.accounts = self.accounts.clone();
        self.config.active_account = self
            .account
            .as_ref()
            .map(|a| a.uuid.clone())
            .unwrap_or_default();
        let _ = config::save(&self.config);
    }

    /// 加入或更新账号（同 UUID 覆盖），并设为当前账号。
    fn upsert_account(&mut self, account: Account) {
        let uuid = account.uuid.clone();
        if let Some(slot) = self.accounts.iter_mut().find(|a| a.uuid == uuid) {
            *slot = account.clone();
        } else {
            self.accounts.push(account.clone());
        }
        self.account = Some(account);
        self.persist_accounts();
    }

    /* --------------------------- 主循环 --------------------------- */

    pub async fn run(&mut self) -> Result<()> {
        let mut terminal = setup_terminal()?;
        let mut events = EventStream::new();
        let mut ticker = tokio::time::interval(Duration::from_millis(100));

        loop {
            terminal.draw(|f| self.draw(f))?;

            // 下载与解压完成后，启动游戏（需退出全屏）
            if let Some((detail, java_path)) = self.pending_launch.take() {
                restore_terminal(&mut terminal)?;
                let opts = launch::LaunchOptions {
                    java: java_path,
                    version_isolation: self.config.version_isolation,
                    ..Default::default()
                };
                let account = self.account.clone().unwrap();
                let result =
                    launch::launch(&self.paths, &detail, &account, &self.env, &opts)
                        .await;
                terminal = setup_terminal()?;
                self.notice = match result {
                    Ok(()) => "游戏已正常退出。\n按 Enter 返回版本列表".to_string(),
                    Err(e) => {
                        format!("游戏异常退出：{e:#}\n按 Enter 返回版本列表")
                    }
                };
                self.screen = Screen::Notice;
            }

            tokio::select! {
                maybe_ev = events.next() => {
                    if let Some(Ok(Event::Key(key))) = maybe_ev
                        && key.kind == KeyEventKind::Press && self.handle_key(key) {
                            break;
                        }
                }
                maybe_app = self.rx.recv() => match maybe_app {
                    Some(ev) => self.handle_app_event(ev),
                    None => break,
                },
                _ = ticker.tick() => {
                    self.spinner = self.spinner.wrapping_add(1);
                }
            }
        }

        restore_terminal(&mut terminal)?;
        Ok(())
    }

    /* --------------------------- 键盘 --------------------------- */

    /// 返回 true 表示退出程序。
    fn handle_key(&mut self, key: KeyEvent) -> bool {
        match self.screen {
            Screen::Loading => {
                if key.code == KeyCode::Char('q') {
                    return true;
                }
            }
            Screen::List => match key.code {
                KeyCode::Char('q') | KeyCode::Char('Q') => return true,
                KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
                KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
                KeyCode::Tab => self.cycle_filter(),
                KeyCode::Char('r') => {
                    self.screen = Screen::Loading;
                    self.reload_manifest();
                }
                KeyCode::Char('o') => {
                    self.input.clear();
                    self.screen = Screen::OfflineInput;
                }
                KeyCode::Char('m') => self.start_microsoft(),
                KeyCode::Enter => self.start_launch(),
                _ => {}
            },
            Screen::OfflineInput => match key.code {
                KeyCode::Esc => self.screen = Screen::List,
                KeyCode::Enter => {
                    let name = if self.input.trim().is_empty() {
                        "Player".to_string()
                    } else {
                        self.input.trim().to_string()
                    };
                    self.upsert_account(Account::offline(&name));
                    self.input.clear();
                    self.screen = Screen::List;
                }
                KeyCode::Backspace => {
                    self.input.pop();
                }
                KeyCode::Char(c)
                    if self.input.chars().count() < 16
                        && (c.is_ascii_alphanumeric() || c == '_')
                    => {
                        self.input.push(c);
                    }
                _ => {}
            },
            Screen::DeviceLogin => {
                if key.code == KeyCode::Esc {
                    if let Some(t) = self.login_task.take() {
                        t.abort();
                    }
                    self.screen = Screen::List;
                }
            }
            Screen::Working => {
                if key.code == KeyCode::Char('q') {
                    return true;
                }
            }
            Screen::Notice => {
                if matches!(
                    key.code,
                    KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q')
                ) {
                    self.screen = Screen::List;
                }
            }
        }
        false
    }

    fn move_selection(&mut self, delta: i32) {
        if self.shown.is_empty() {
            return;
        }
        let next = self.selected as i32 + delta;
        self.selected = next.clamp(0, self.shown.len() as i32 - 1) as usize;
    }

    fn cycle_filter(&mut self) {
        self.filter = (self.filter + 1) % 3;
        if let Some(m) = &self.manifest {
            self.shown = filter_versions(m, self.filter);
            self.merge_local();
            self.selected = 0;
        }
    }

    /// 把本地 versions 目录中已登记的版本（加载器/整合包）追加到列表。
    fn merge_local(&mut self) {
        let Ok(entries) = std::fs::read_dir(self.paths.versions_dir()) else {
            return;
        };
        for e in entries.flatten() {
            if !e.path().is_dir() {
                continue;
            }
            let Some(name) = e.file_name().to_str().map(|s| s.to_string()) else {
                continue;
            };
            if !self.paths.version_json(&name).exists() {
                continue;
            }
            if self.shown.iter().any(|v| v.id == name) {
                continue;
            }
            self.shown.push(VersionEntry {
                id: name,
                version_type: "custom".to_string(),
                url: String::new(),
                sha1: String::new(),
                release_time: String::new(),
            });
        }
    }

    /* --------------------------- 后台任务 --------------------------- */

    fn reload_manifest(&self) {
        let tx = self.tx.clone();
        let client = self.client.clone();
        tokio::spawn(async move {
            match manifest::fetch_manifest(&client).await {
                Ok(m) => {
                    let _ = tx.send(AppEvent::ManifestLoaded(m));
                }
                Err(e) => {
                    let _ = tx.send(AppEvent::ManifestError(format!("{e:#}")));
                }
            }
        });
    }

    fn start_microsoft(&mut self) {
        let tx = self.tx.clone();
        let client = self.client.clone();
        let task = tokio::spawn(async move {
            let device = match auth::request_device_code(&client).await {
                Ok(d) => d,
                Err(e) => {
                    let _ = tx.send(AppEvent::AuthError(format!("{e:#}")));
                    return;
                }
            };
            let device2 = device.clone();
            let _ = tx.send(AppEvent::DeviceCode(device));
            let result = auth::complete_device_login(&client, device2, |msg| {
                let _ = tx.send(AppEvent::Status(msg.to_string()));
            })
            .await;
            match result {
                Ok(a) => {
                    let _ = tx.send(AppEvent::Authed(a));
                }
                Err(e) => {
                    let _ = tx.send(AppEvent::AuthError(format!("{e:#}")));
                }
            }
        });
        self.login_task = Some(task);
        self.screen = Screen::DeviceLogin;
    }

    fn start_launch(&mut self) {
        if self.account.is_none() {
            self.input.clear();
            self.screen = Screen::OfflineInput;
            return;
        }
        let Some(entry) = self.shown.get(self.selected).cloned() else {
            return;
        };
        let manifest = self.manifest.clone();
        let installed = self.paths.version_json(&entry.id).exists();
        if !installed && manifest.is_none() {
            return;
        }

        let tx = self.tx.clone();
        let client = self.client.clone();
        let paths = self.paths.clone();
        let env = self.env.clone();

        let (dl_tx, dl_rx) = mpsc::unbounded_channel::<DownloadEvent>();
        let fwd_tx = tx.clone();
        tokio::spawn(async move {
            let mut dl_rx = dl_rx;
            while let Some(e) = dl_rx.recv().await {
                let _ = fwd_tx.send(AppEvent::Download(e));
            }
        });

        tokio::spawn(async move {
            let _ = tx.send(AppEvent::Status("正在解析版本…".to_string()));
            let detail = match manifest::load_resolved_version(
                &entry.id, &paths, &client, manifest.as_ref(),
            )
            .await
            {
                Ok(d) => d,
                Err(e) => {
                    let _ = tx.send(AppEvent::LaunchError(format!("{e:#}")));
                    return;
                }
            };

            let _ = tx.send(AppEvent::Status("正在收集文件清单…".to_string()));
            let (tasks, _) =
                match rustmc_launcher::core::download::collect_tasks(
                    &client, &paths, &detail, &env,
                )
                    .await
                {
                    Ok(t) => t,
                    Err(e) => {
                        let _ = tx.send(AppEvent::LaunchError(format!("{e:#}")));
                        return;
                    }
                };

            if let Err(e) =
                rustmc_launcher::core::download::run(
                    client.clone(),
                    tasks,
                    dl_tx,
                    16,
                )
                .await
            {
                let _ = tx.send(AppEvent::LaunchError(format!("{e:#}")));
                return;
            }

            let _ = tx.send(AppEvent::Status("正在解压原生库…".to_string()));
            if let Err(e) =
                rustmc_launcher::core::download::extract_natives(
                    &paths, &detail, &env,
                )
            {
                let _ = tx.send(AppEvent::LaunchError(format!("{e:#}")));
                return;
            }

            let required = java::required_major(&detail);
            let available = java::detect_all();
            let Some(picked) = java::pick_compatible(&available, required) else {
                let _ = tx.send(AppEvent::LaunchError(format!(
                    "未找到 Java（该版本需要 Java {required}），请安装后重试"
                )));
                return;
            };

            let _ = tx.send(AppEvent::ReadyToLaunch(Box::new(detail), picked.path));
        });

        self.screen = Screen::Working;
    }

    fn handle_app_event(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::ManifestLoaded(m) => {
                self.shown = filter_versions(&m, self.filter);
                self.merge_local();
                self.manifest = Some(m);
                if self.screen == Screen::Loading {
                    self.screen = Screen::List;
                }
            }
            AppEvent::ManifestError(e) => {
                self.notice = format!("加载版本清单失败：{e}\n按 r 重试");
                self.screen = Screen::Notice;
            }
            AppEvent::Status(s) => self.status = s,
            AppEvent::Download(e) => self.handle_download(e),
            AppEvent::DeviceCode(d) => {
                self.device = Some(d);
                self.status = "等待浏览器授权".to_string();
            }
            AppEvent::Authed(a) => {
                self.upsert_account(a);
                self.login_task = None;
                self.screen = Screen::List;
            }
            AppEvent::AuthError(e) => {
                self.login_task = None;
                self.notice = format!("登录失败：{e}\n按 Enter 返回");
                self.screen = Screen::Notice;
            }
            AppEvent::ReadyToLaunch(detail, jp) => {
                // 版本文件已下载完成，刷新本地列表。
                match &self.manifest {
                    Some(m) => {
                        self.shown = filter_versions(m, self.filter);
                        self.merge_local();
                    }
                    None => {
                        self.shown = Vec::new();
                        self.merge_local();
                    }
                }
                self.pending_launch = Some((*detail, jp));
            }
            AppEvent::LaunchError(e) => {
                self.notice = format!("启动失败：{e}\n按 Enter 返回");
                self.screen = Screen::Notice;
            }
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
            DownloadEvent::Finish => self.status = "文件就绪".to_string(),
        }
    }

    /* --------------------------- 渲染 --------------------------- */

    fn draw(&self, f: &mut Frame) {
        // 每帧先清屏，避免进入备用屏幕前的内容残留
        f.render_widget(Clear, f.area());

        let working = self.screen == Screen::Working;
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(5),
                Constraint::Length(if working { 5 } else { 1 }),
            ])
            .split(f.area());

        self.draw_header(f, chunks[0]);
        self.draw_body(f, chunks[1]);
        if working {
            self.draw_footer(f, chunks[2]);
        } else {
            self.draw_keys(f, chunks[2]);
        }
    }

    fn draw_header(&self, f: &mut Frame, area: Rect) {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(area);

        let title = Line::from(vec![
            Span::styled(
                " RustMC ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                " Launcher ",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("v{}", env!("CARGO_PKG_VERSION")),
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        f.render_widget(Paragraph::new(title), cols[0]);

        let account = match &self.account {
            Some(a) => Line::from(vec![
                Span::styled("● ", Style::default().fg(Color::Green)),
                Span::styled(
                    &a.name,
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" {}", a.kind_label()),
                    Style::default().fg(Color::DarkGray),
                ),
            ]),
            None => Line::from(vec![
                Span::styled("○ ", Style::default().fg(Color::Yellow)),
                Span::styled("未登录", Style::default().fg(Color::Yellow)),
            ]),
        };
        f.render_widget(
            Paragraph::new(account).alignment(Alignment::Right),
            cols[1],
        );
    }

    fn draw_body(&self, f: &mut Frame, area: Rect) {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
            .split(area);
        self.draw_list(f, cols[0]);
        self.draw_panel(f, cols[1]);
    }

    fn draw_list(&self, f: &mut Frame, area: Rect) {
        let filter_name = ["全部", "正式版", "快照版"][self.filter as usize];
        let block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(
                format!(" 版本列表 [{filter_name}] "),
                Style::default().fg(Color::Cyan),
            ));

        let items: Vec<ListItem> = self
            .shown
            .iter()
            .map(|v| {
                let (marker, color) = match v.version_type.as_str() {
                    "release" => ("R", Color::Green),
                    "snapshot" => ("S", Color::Yellow),
                    "custom" => ("C", Color::Cyan),
                    _ => ("O", Color::Blue),
                };
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!(" {marker} "),
                        Style::default()
                            .fg(color)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(&v.id),
                ]))
            })
            .collect();

        let list = List::new(items)
            .block(block)
            .highlight_symbol("▶ ")
            .highlight_style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            );
        let mut state = ListState::default();
        state.select(Some(self.selected.min(self.shown.len().saturating_sub(1))));
        f.render_stateful_widget(list, area, &mut state);
    }

    fn draw_panel(&self, f: &mut Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(" 状态 ", Style::default().fg(Color::Cyan)));
        let inner = block.inner(area);
        f.render_widget(block, area);

        let content: Vec<Line> = match self.screen {
            Screen::Loading => vec![Line::from(vec![
                Span::styled(spinner(self.spinner), Style::default().fg(Color::Cyan)),
                Span::raw(" 正在加载版本清单…"),
            ])],
            Screen::List => {
                if let Some(v) = self.shown.get(self.selected) {
                    vec![
                        Line::from(vec![
                            Span::styled("版本: ", Style::default().fg(Color::DarkGray)),
                            Span::styled(
                                &v.id,
                                Style::default()
                                    .fg(Color::White)
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ]),
                        Line::from(vec![
                            Span::styled("类型: ", Style::default().fg(Color::DarkGray)),
                            Span::raw(&v.version_type),
                        ]),
                        Line::from(vec![
                            Span::styled("发布: ", Style::default().fg(Color::DarkGray)),
                            Span::raw(&v.release_time),
                        ]),
                        Line::from(""),
                        Line::from(Span::styled(
                            "按 Enter 下载并启动",
                            Style::default().fg(Color::Green),
                        )),
                    ]
                } else {
                    vec![Line::from("暂无版本")]
                }
            }
            Screen::OfflineInput => vec![
                Line::from(Span::styled(
                    "离线登录（无需联网）",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(vec![
                    Span::styled("> ", Style::default().fg(Color::Green)),
                    Span::styled(&self.input, Style::default().fg(Color::White)),
                    Span::styled("_", Style::default().fg(Color::Green)),
                ]),
                Line::from(""),
                Line::from(Span::styled(
                    "玩家名：字母/数字/下划线，最多 16 字符",
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(Span::styled(
                    "Enter 确认，Esc 返回",
                    Style::default().fg(Color::DarkGray),
                )),
            ],
            Screen::DeviceLogin => {
                let mut lines = vec![
                    Line::from(Span::styled(
                        "微软账号登录",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )),
                    Line::from(""),
                ];
                if let Some(d) = &self.device {
                    lines.push(Line::from(vec![
                        Span::styled("1. 打开网页: ", Style::default().fg(Color::DarkGray)),
                        Span::styled(&d.verification_uri, Style::default().fg(Color::Cyan)),
                    ]));
                    lines.push(Line::from(vec![
                        Span::styled("2. 输入代码: ", Style::default().fg(Color::DarkGray)),
                        Span::styled(
                            &d.user_code,
                            Style::default()
                                .fg(Color::Green)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    lines.push(Line::from(""));
                }
                lines.push(Line::from(vec![
                    Span::styled(spinner(self.spinner), Style::default().fg(Color::Cyan)),
                    Span::raw(format!(" {}", self.status)),
                ]));
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "Esc 取消登录",
                    Style::default().fg(Color::DarkGray),
                )));
                lines
            }
            Screen::Working => vec![
                Line::from(vec![
                    Span::styled(spinner(self.spinner), Style::default().fg(Color::Cyan)),
                    Span::raw(format!(" {}", self.status)),
                ]),
                Line::from(""),
                Line::from(vec![
                    Span::styled("当前文件: ", Style::default().fg(Color::DarkGray)),
                    Span::raw(&self.progress.current),
                ]),
            ],
            Screen::Notice => self
                .notice
                .lines()
                .map(|l| {
                    Line::from(Span::styled(
                        l.to_string(),
                        Style::default().fg(Color::Yellow),
                    ))
                })
                .collect(),
        };
        f.render_widget(
            Paragraph::new(content).wrap(Wrap { trim: false }),
            inner,
        );
    }

    fn draw_footer(&self, f: &mut Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(
                " 下载进度 ",
                Style::default().fg(Color::Cyan),
            ));
        let inner = block.inner(area);
        f.render_widget(block, area);

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(inner);

        let p = &self.progress;
        let pct = if p.total_files > 0 {
            p.completed_files as f64 / p.total_files as f64
        } else {
            0.0
        };
        let bar_width = (rows[0].width as usize).saturating_sub(8);
        let filled = (pct * bar_width as f64) as usize;
        let bar = format!(
            "{}{} {:>3.0}%",
            "█".repeat(filled),
            "░".repeat(bar_width - filled),
            pct * 100.0
        );
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                bar,
                Style::default().fg(Color::Green),
            ))),
            rows[0],
        );

        let info = format!(
            "文件 {}/{}   {:.1}/{:.1} MB",
            p.completed_files,
            p.total_files,
            p.done_bytes as f64 / 1_048_576.0,
            p.total_bytes as f64 / 1_048_576.0
        );
        f.render_widget(
            Paragraph::new(Span::styled(info, Style::default().fg(Color::DarkGray))),
            rows[1],
        );
    }

    fn draw_keys(&self, f: &mut Frame, area: Rect) {
        let keys = Line::from(vec![
            Span::styled("Enter", Style::default().fg(Color::Green)),
            Span::styled(" 启动  ", Style::default().fg(Color::DarkGray)),
            Span::styled("o", Style::default().fg(Color::Green)),
            Span::styled(" 离线  ", Style::default().fg(Color::DarkGray)),
            Span::styled("m", Style::default().fg(Color::Green)),
            Span::styled(" 微软  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Tab", Style::default().fg(Color::Green)),
            Span::styled(" 筛选  ", Style::default().fg(Color::DarkGray)),
            Span::styled("r", Style::default().fg(Color::Green)),
            Span::styled(" 刷新  ", Style::default().fg(Color::DarkGray)),
            Span::styled("q", Style::default().fg(Color::Red)),
            Span::styled(" 退出", Style::default().fg(Color::DarkGray)),
        ]);
        f.render_widget(
            Paragraph::new(keys).alignment(Alignment::Center),
            area,
        );
    }
}

fn filter_versions(m: &VersionManifest, filter: u8) -> Vec<VersionEntry> {
    m.versions
        .iter()
        .filter(|v| match filter {
            1 => v.version_type == "release",
            2 => v.version_type == "snapshot",
            _ => true,
        })
        .cloned()
        .collect()
}

/* --------------------------- 终端控制 --------------------------- */

pub fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() {
        anyhow::bail!(
            "当前不是交互式终端（TTY），请在真实终端中直接运行本程序，\n不要通过管道或在无终端的环境中启动。"
        );
    }
    if let Err(e) = enable_raw_mode() {
        let _ = disable_raw_mode();
        return Err(e.into());
    }
    if let Err(e) = execute!(stdout(), EnterAlternateScreen) {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen);
        return Err(e.into());
    }
    // 显式清屏：无论终端是否在切换备用屏幕时自动清空，都保证初始画面干净
    if let Err(e) = execute!(stdout(), TermClear(ClearType::All)) {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen);
        return Err(e.into());
    }
    let backend = CrosstermBackend::new(stdout());
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

pub fn restore_terminal(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

/// TUI 独立入口（图形界面为主程序，此为可选终端版本）。
#[tokio::main]
async fn main() -> Result<()> {
    let config = config::load();
    let paths = config.paths();
    let _ = paths.ensure_dirs();

    // 首次启动：提示游戏数据目录，仅出现一次。
    if !config.first_run_done {
        println!("========================================");
        println!("  欢迎使用 RustMC 终端版");
        println!("========================================");
        println!("游戏数据目录：{}", paths.base_dir.display());
        println!("（存档/模组/资源包等将存放在此；可在设置中修改）");
        println!();
        print!("按 Enter 继续…");
        use std::io::Write;
        std::io::stdout().flush()?;
        let mut s = String::new();
        std::io::stdin().read_line(&mut s)?;
        let mut c = config;
        c.first_run_done = true;
        let _ = config::save(&c);
    } else {
        // 仍保留 config 给 App::new（App 内部会重新 load，这里只是标记）。
    }

    let env = EnvInfo::current();
    let client = reqwest::Client::builder()
        .user_agent("RustMC-Launcher/0.1")
        .build()?;
    let mut app = App::new(paths, client, env);
    app.run().await
}
