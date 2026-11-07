//! 启动器配置：游戏目录、下载线程数、内存、自定义 Java 与额外启动参数。
//!
//! 配置文件固定存放在系统配置目录（`rustmc-launcher/config.json`），
//! 与游戏目录解耦，切换游戏目录后设置不丢失。

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::core::auth::Account;
use crate::core::paths::{LauncherPaths, default_minecraft_dir};

/// 启动器持久化配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LauncherConfig {
    /// 自定义游戏目录（.minecraft），空串表示使用默认位置
    #[serde(default)]
    pub game_dir: String,
    /// 下载并发线程数：0 = 自动（按 CPU 核数），范围 1..=256
    #[serde(default = "default_threads")]
    pub download_threads: u32,
    /// 最小内存（MB）
    #[serde(default = "default_min_mem")]
    pub min_memory_mb: u32,
    /// 最大内存（MB）
    #[serde(default = "default_max_mem")]
    pub max_memory_mb: u32,
    /// 自定义 Java 可执行文件路径，空串表示自动检测
    #[serde(default)]
    pub custom_java: String,
    /// 额外 JVM 参数（原始字符串，按空白拆分，支持引号）
    #[serde(default)]
    pub extra_jvm_args: String,
    /// 额外游戏参数（原始字符串，如 `--server mc.example.com --port 25565`）
    #[serde(default)]
    pub extra_game_args: String,
    /// 游戏窗口宽度（0 表示不设置）
    #[serde(default)]
    pub window_width: u32,
    /// 游戏窗口高度（0 表示不设置）
    #[serde(default)]
    pub window_height: u32,
    /// 启动后自动进入全屏
    #[serde(default)]
    pub fullscreen: bool,
    /// 版本隔离（默认开启 = 全部隔离）：每个版本使用独立目录 `versions/<版本号>`，
    /// 存档 / 模组 / 资源包 / 光影 / 配置互不影响；关闭则共用游戏根目录。
    #[serde(default = "default_version_isolation")]
    pub version_isolation: bool,
    /// 本地保存的账号列表（离线 / 微软 / 第三方），永久保留在本地。
    #[serde(default)]
    pub accounts: Vec<Account>,
    /// 当前选中账号的 UUID（空串表示未选择）。
    #[serde(default)]
    pub active_account: String,
    /// 首次启动引导是否已完成（选过游戏目录后置 true，不再询问）。
    #[serde(default)]
    pub first_run_done: bool,
    /// 外部启动器目录（HMCL / PCL / 官方 .minecraft 等），直接挂载读取已下载版本与存档。
    /// 启动时会自动检测常见位置并合并到这里。
    #[serde(default)]
    pub external_dirs: Vec<String>,
    /// AI 助手：OpenAI 兼容接口的 Base URL（如 https://api.deepseek.com 、https://ark.cn-beijing.volces.com/api/v3）。
    #[serde(default = "default_ai_base")]
    pub ai_api_base: String,
    /// AI 助手：API Key（仅保存在本地配置文件）。
    #[serde(default)]
    pub ai_api_key: String,
    /// AI 助手：模型名（如 deepseek-chat、doubao-pro-32k、gpt-4o-mini）。
    #[serde(default = "default_ai_model")]
    pub ai_model: String,
}

fn default_ai_base() -> String {
    "https://api.deepseek.com".to_string()
}

fn default_ai_model() -> String {
    "deepseek-chat".to_string()
}

fn default_threads() -> u32 {
    16
}
fn default_version_isolation() -> bool {
    true
}
fn default_min_mem() -> u32 {
    512
}
fn default_max_mem() -> u32 {
    2048
}

impl Default for LauncherConfig {
    fn default() -> Self {
        LauncherConfig {
            game_dir: String::new(),
            download_threads: default_threads(),
            min_memory_mb: default_min_mem(),
            max_memory_mb: default_max_mem(),
            custom_java: String::new(),
            extra_jvm_args: String::new(),
            extra_game_args: String::new(),
            window_width: 0,
            window_height: 0,
            fullscreen: false,
            version_isolation: true,
            accounts: Vec::new(),
            active_account: String::new(),
            first_run_done: false,
            external_dirs: Vec::new(),
            ai_api_base: default_ai_base(),
            ai_api_key: String::new(),
            ai_model: default_ai_model(),
        }
    }
}

impl LauncherConfig {
    /// 解析配置得到实际的游戏目录路径。
    pub fn resolved_game_dir(&self) -> PathBuf {
        let trimmed = self.game_dir.trim();
        if trimmed.is_empty() {
            default_minecraft_dir()
        } else {
            PathBuf::from(trimmed)
        }
    }

    /// 根据配置构造路径助手。
    pub fn paths(&self) -> LauncherPaths {
        LauncherPaths::new(self.resolved_game_dir())
    }

    /// 实际生效的下载并发数。
    ///
    /// `0` 表示自动：取 CPU 逻辑核数（最少 4，最多 64）；
    /// 手动值会被夹到 1..=256。
    pub fn effective_threads(&self) -> usize {
        if self.download_threads == 0 {
            let n = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(8);
            n.clamp(4, 64)
        } else {
            self.download_threads.clamp(1, 256) as usize
        }
    }

    /// 把额外 JVM 参数字符串拆分为参数列表（支持单/双引号）。
    pub fn extra_jvm_args_list(&self) -> Vec<String> {
        split_args(&self.extra_jvm_args)
    }

    /// 把额外游戏参数字符串拆分为参数列表。
    pub fn extra_game_args_list(&self) -> Vec<String> {
        split_args(&self.extra_game_args)
    }

    /// 当前生效账号（按 `active_account` 的 UUID 查找，找不到返回 None）。
    pub fn active(&self) -> Option<&Account> {
        self.accounts.iter().find(|a| a.uuid == self.active_account)
    }
}

/// 配置文件所在目录（系统配置目录下的 `rustmc-launcher`）。
pub fn config_dir() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".config")
    });
    base.join("rustmc-launcher")
}

/// 配置文件完整路径。
pub fn config_file() -> PathBuf {
    config_dir().join("config.json")
}

/// 读取配置；文件不存在或损坏时返回默认值（损坏不报错，避免打不开启动器）。
pub fn load() -> LauncherConfig {
    let path = config_file();
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => LauncherConfig::default(),
    }
}

/// 保存配置到磁盘。
pub fn save(config: &LauncherConfig) -> Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("创建配置目录失败：{}", dir.display()))?;
    let text = serde_json::to_string_pretty(config).context("序列化配置失败")?;
    std::fs::write(config_file(), text).context("写入配置文件失败")?;
    Ok(())
}

/// 极简命令行参数拆分：按空白分词，支持 `"..."` 与 `'...'` 包裹含空格的参数。
pub fn split_args(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;

    for ch in input.chars() {
        match quote {
            Some(q) => {
                if ch == q {
                    quote = None;
                } else {
                    current.push(ch);
                }
            }
            None => match ch {
                '"' | '\'' => {
                    quote = Some(ch);
                    started = true;
                }
                c if c.is_whitespace() => {
                    if started {
                        out.push(std::mem::take(&mut current));
                        started = false;
                    }
                }
                c => {
                    current.push(c);
                    started = true;
                }
            },
        }
    }
    if started {
        out.push(current);
    }
    out
}
