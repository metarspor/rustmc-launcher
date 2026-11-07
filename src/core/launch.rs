//! 游戏启动：classpath、JVM/游戏参数组装、占位符替换与进程拉起。

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use tokio::process::Command;

use crate::core::auth::Account;
use crate::core::manifest::VersionDetail;
use crate::core::paths::LauncherPaths;
use crate::core::rules::{self, EnvInfo};

/// 启动选项。
#[derive(Debug, Clone)]
pub struct LaunchOptions {
    pub java: PathBuf,
    pub min_memory_mb: u32,
    pub max_memory_mb: u32,
    /// 额外 JVM 参数（用户自定义，如 `-Dfile.encoding=UTF-8 -XX:+UseG1GC`）
    pub extra_jvm_args: Vec<String>,
    /// 额外游戏参数（用户自定义，如 `--server mc.example.com --port 25565`）
    pub extra_game_args: Vec<String>,
    /// 窗口宽度（0 表示不设置）
    pub window_width: u32,
    /// 窗口高度（0 表示不设置）
    pub window_height: u32,
    /// 是否全屏启动
    pub fullscreen: bool,
    /// 版本隔离：开启后游戏数据目录指向 `versions/<版本号>`，
    /// 存档 / 模组 / 资源包 / 光影 / 配置按版本独立存放。
    pub version_isolation: bool,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            java: PathBuf::from("java"),
            min_memory_mb: 512,
            max_memory_mb: 2048,
            extra_jvm_args: Vec::new(),
            extra_game_args: Vec::new(),
            window_width: 0,
            window_height: 0,
            fullscreen: false,
            version_isolation: true,
        }
    }
}

/// 组装参数并启动游戏，等待游戏退出。
///
/// 调用方应在启动前退出 TUI 全屏，让游戏直接接管终端输入输出。
pub async fn launch(
    paths: &LauncherPaths,
    detail: &VersionDetail,
    account: &Account,
    env: &EnvInfo,
    opts: &LaunchOptions,
) -> Result<()> {
    let game_dir = game_dir(paths, detail, opts);
    prepare_game_dir(&game_dir, opts.version_isolation)?;

    let (jvm, main_class, game) =
        build_arguments(paths, detail, account, env, opts, &game_dir)?;

    // ---- 拉起进程 ----
    let mut cmd = Command::new(&opts.java);
    cmd.args(&jvm).arg(&main_class).args(&game);
    cmd.current_dir(&game_dir);

    let status = cmd
        .status()
        .await
        .map_err(|e| anyhow::anyhow!("无法启动 Java 进程：{e}"))?;

    if !status.success() {
        bail!("游戏异常退出（退出码 {}）", status.code().unwrap_or(-1));
    }
    Ok(())
}

/// 实际游戏数据目录：开启版本隔离时为 `versions/<版本号>`，否则为游戏根目录。
fn game_dir(paths: &LauncherPaths, detail: &VersionDetail, opts: &LaunchOptions) -> PathBuf {
    resolve_game_dir(paths, &detail.id, opts.version_isolation)
}

/// 隔离目录决策纯函数：开启隔离返回 `versions/<id>`，否则返回根目录。
fn resolve_game_dir(paths: &LauncherPaths, id: &str, isolated: bool) -> PathBuf {
    if isolated {
        paths.version_dir(id)
    } else {
        paths.base_dir.clone()
    }
}

/// 确保游戏目录存在；隔离模式下预建常见数据子目录（存档 / 模组 / 资源包 / 光影 / 配置）。
fn prepare_game_dir(dir: &std::path::Path, isolated: bool) -> Result<()> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("创建游戏目录失败：{}", dir.display()))?;
    if isolated {
        for sub in [
            "saves",
            "mods",
            "resourcepacks",
            "shaderpacks",
            "config",
            "screenshots",
        ] {
            let _ = std::fs::create_dir_all(dir.join(sub));
        }
    }
    Ok(())
}

/// 构造完整启动参数：(JVM 参数, 主类, 游戏参数)。
pub fn build_arguments(
    paths: &LauncherPaths,
    detail: &VersionDetail,
    account: &Account,
    env: &EnvInfo,
    opts: &LaunchOptions,
    game_dir: &std::path::Path,
) -> Result<(Vec<String>, String, Vec<String>)> {
    let natives_dir = paths.natives_dir(&detail.id);
    let classpath = build_classpath(paths, detail, env)?;
    let replacements =
        build_replacements(paths, detail, account, &classpath, &natives_dir, game_dir);

    // ---- JVM 参数 ----
    let mut jvm: Vec<String> = vec![
        format!("-Xmx{}M", opts.max_memory_mb),
        format!("-Xms{}M", opts.min_memory_mb),
    ];

    // 用户自定义 JVM 参数（紧随内存参数，便于覆盖/补充）
    jvm.extend(opts.extra_jvm_args.iter().cloned());

    if let Some(arguments) = &detail.arguments {
        if let Some(jvm_defs) = &arguments.jvm {
            for arg in jvm_defs {
                for raw in arg.expand(env) {
                    jvm.push(apply(&raw, &replacements));
                }
            }
        }
    } else {
        // 旧版（1.12 及更早）手动 JVM 参数
        jvm.push(format!("-Djava.library.path={}", natives_dir.display()));
        if cfg!(target_os = "macos") {
            jvm.push("-XstartOnFirstThread".to_string());
        }
        jvm.push("-cp".to_string());
        jvm.push(classpath.clone());
    }

    // log4j 配置（现代版本）
    if let Some(logging) = &detail.logging
        && let Some(client) = &logging.client {
        let id = client
            .file
            .id
            .clone()
            .unwrap_or_else(|| "client.xml".to_string());
        let path = paths.log_configs_dir().join(id);
        jvm.push(client.argument.replace("${path}", &path.to_string_lossy()));
    }

    // ---- 游戏参数 ----
    let mut game: Vec<String> = Vec::new();
    if let Some(arguments) = &detail.arguments {
        for arg in &arguments.game {
            for raw in arg.expand(env) {
                game.push(apply(&raw, &replacements));
            }
        }
    } else if let Some(mc_args) = &detail.minecraft_arguments {
        for raw in mc_args.split_whitespace() {
            game.push(apply(raw, &replacements));
        }
    }

    // 窗口尺寸（覆盖版本 JSON 中的默认分辨率）
    if opts.window_width > 0 && opts.window_height > 0 {
        game.push("--width".to_string());
        game.push(opts.window_width.to_string());
        game.push("--height".to_string());
        game.push(opts.window_height.to_string());
    }
    if opts.fullscreen {
        game.push("--fullscreen".to_string());
    }

    // 用户自定义游戏参数（放在最后，如 --server/--port 快速加入）
    game.extend(opts.extra_game_args.iter().cloned());

    let main_class = detail
        .main_class
        .clone()
        .unwrap_or_else(|| "net.minecraft.client.Minecraft".to_string());

    Ok((jvm, main_class, game))
}

/// 构造 classpath：所有匹配的库 + 客户端 jar。
fn build_classpath(
    paths: &LauncherPaths,
    detail: &VersionDetail,
    env: &EnvInfo,
) -> Result<String> {
    let mut entries: Vec<String> = Vec::new();

    for lib in detail.libraries.iter().flatten() {
        if !rules::allowed(lib.rules.as_ref(), env) {
            continue;
        }
        // native jar（独立的 `:natives-*` 条目）也必须加入 classpath：
        // LWJGL 3.3+ 会从 classpath 上的 native jar 自动提取 .so 到
        // SharedLibraryExtractPath；jtracy 等也通过 ClassLoader 加载 native。
        if let Some(rel) = lib
            .downloads
            .as_ref()
            .and_then(|d| d.artifact.as_ref())
            .and_then(|a| a.path.as_ref())
        {
            entries.push(paths.libraries_dir().join(rel).display().to_string());
        } else if lib.url.is_some()
            && let Some(rel) = LauncherPaths::library_rel_path(&lib.name) {
                entries.push(paths.libraries_dir().join(rel).display().to_string());
            }
    }

    entries.push(paths.version_jar(&detail.id).display().to_string());
    Ok(entries.join(classpath_separator()))
}

fn classpath_separator() -> &'static str {
    if cfg!(target_os = "windows") {
        ";"
    } else {
        ":"
    }
}

fn build_replacements(
    paths: &LauncherPaths,
    detail: &VersionDetail,
    account: &Account,
    classpath: &str,
    natives_dir: &std::path::Path,
    game_dir: &std::path::Path,
) -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("natives_directory".to_string(), natives_dir.display().to_string());
    m.insert("launcher_name".to_string(), "RustMC-Launcher".to_string());
    m.insert(
        "launcher_version".to_string(),
        env!("CARGO_PKG_VERSION").to_string(),
    );
    m.insert("classpath".to_string(), classpath.to_string());
    m.insert(
        "library_directory".to_string(),
        paths.libraries_dir().display().to_string(),
    );
    m.insert(
        "classpath_separator".to_string(),
        classpath_separator().to_string(),
    );

    m.insert("auth_player_name".to_string(), account.name.clone());
    m.insert("version_name".to_string(), detail.id.clone());
    m.insert(
        "game_directory".to_string(),
        game_dir.display().to_string(),
    );
    m.insert(
        "assets_root".to_string(),
        paths.assets_dir().display().to_string(),
    );
    m.insert(
        "game_assets".to_string(),
        paths
            .assets_dir()
            .join("virtual")
            .join(detail.assets.as_deref().unwrap_or("legacy"))
            .display()
            .to_string(),
    );
    m.insert(
        "assets_index_name".to_string(),
        detail.assets.clone().unwrap_or_default(),
    );
    m.insert("auth_uuid".to_string(), account.uuid.clone());
    m.insert(
        "auth_access_token".to_string(),
        account.access_token.clone(),
    );
    m.insert("access_token".to_string(), account.access_token.clone());
    m.insert("clientid".to_string(), String::new());
    m.insert("auth_xuid".to_string(), String::new());
    m.insert("user_type".to_string(), account.user_type().to_string());
    m.insert(
        "version_type".to_string(),
        detail.version_type.clone().unwrap_or_else(|| "release".to_string()),
    );
    m.insert("user_properties".to_string(), "{}".to_string());
    m.insert("resolution_width".to_string(), "854".to_string());
    m.insert("resolution_height".to_string(), "480".to_string());
    m
}

fn apply(raw: &str, replacements: &HashMap<String, String>) -> String {
    let mut out = raw.to_string();
    for (key, value) in replacements {
        out = out.replace(&format!("${{{key}}}"), value);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::paths::LauncherPaths;

    #[test]
    fn resolve_game_dir_non_isolated_uses_root() {
        let tmp = std::env::temp_dir().join("rustmc_iso_n");
        let paths = LauncherPaths::new(&tmp);
        assert_eq!(resolve_game_dir(&paths, "1.20.1", false), paths.base_dir);
    }

    #[test]
    fn resolve_game_dir_isolated_uses_version_dir() {
        let tmp = std::env::temp_dir().join("rustmc_iso_y");
        let paths = LauncherPaths::new(&tmp);
        assert_eq!(
            resolve_game_dir(&paths, "1.20.1", true),
            paths.version_dir("1.20.1")
        );
    }

    #[test]
    fn prepare_game_dir_isolated_creates_data_subdirs() {
        let tmp = std::env::temp_dir().join("rustmc_iso_prep");
        let _ = std::fs::remove_dir_all(&tmp);
        let paths = LauncherPaths::new(&tmp);
        let dir = paths.version_dir("1.20.1");
        prepare_game_dir(&dir, true).unwrap();
        for sub in [
            "saves",
            "mods",
            "resourcepacks",
            "shaderpacks",
            "config",
            "screenshots",
        ] {
            assert!(dir.join(sub).is_dir(), "缺少子目录 {sub}");
        }
    }
}
