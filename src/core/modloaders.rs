//! 模组加载器安装：Fabric / Quilt / Forge / NeoForge / OptiFine。
//!
//! - Fabric / Quilt：从官方 meta 拉取版本 profile JSON 写入 `versions/`，
//!   其余库文件在启动时随常规下载流程完成。
//! - Forge / NeoForge：下载官方安装器，使用 Java 以 `--installClient` 静默安装。
//! - OptiFine：从 BMCLAPI（或官网）下载安装器，启动官方安装器完成安装。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::core::download::{self, DownloadEvent, DownloadTask};
use crate::core::java::{self, JavaInfo};
use crate::core::paths::LauncherPaths;

/// 安装过程中的消息：状态文本或下载事件。
#[derive(Debug, Clone)]
pub enum InstallMessage {
    Status(String),
    Download(DownloadEvent),
}

/* ----------------------------- 数据结构 ----------------------------- */

/// 加载器种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoaderKind {
    Fabric,
    Quilt,
    Forge,
    NeoForge,
    OptiFine,
}

impl LoaderKind {
    pub fn label(self) -> &'static str {
        match self {
            LoaderKind::Fabric => "Fabric",
            LoaderKind::Quilt => "Quilt",
            LoaderKind::Forge => "Forge",
            LoaderKind::NeoForge => "NeoForge",
            LoaderKind::OptiFine => "OptiFine",
        }
    }

    pub fn all() -> [LoaderKind; 5] {
        [
            LoaderKind::Fabric,
            LoaderKind::Quilt,
            LoaderKind::Forge,
            LoaderKind::NeoForge,
            LoaderKind::OptiFine,
        ]
    }
}

/// 一个可选的加载器版本。
#[derive(Debug, Clone)]
pub struct LoaderVersion {
    /// 版本号（如 `0.16.10` / `47.2.0` / `21.1.0` / `HD_U I6`）
    pub version: String,
    /// 是否稳定版
    pub stable: bool,
}

/// OptiFine 版本条目（BMCLAPI 结构）。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct OptiFineEntry {
    pub mcversion: String,
    #[serde(rename = "type")]
    pub of_type: String,
    pub patch: String,
    pub filename: String,
    #[serde(default)]
    pub forge: String,
}

/* ----------------------------- 版本列表 ----------------------------- */

/// Fabric loader 版本列表（按时间倒序，最新在前）。
pub async fn list_fabric(client: &reqwest::Client, mc: &str) -> Result<Vec<LoaderVersion>> {
    let url = format!("https://meta.fabricmc.net/v2/versions/loader/{mc}");
    let data: Vec<serde_json::Value> = get_json(client, &url).await?;
    Ok(data
        .iter()
        .filter_map(|v| {
            let loader = v.get("loader")?;
            Some(LoaderVersion {
                version: loader.get("version")?.as_str()?.to_string(),
                stable: loader.get("stable").and_then(|s| s.as_bool()).unwrap_or(true),
            })
        })
        .collect())
}

/// Quilt loader 版本列表。
pub async fn list_quilt(client: &reqwest::Client, mc: &str) -> Result<Vec<LoaderVersion>> {
    let url = format!("https://meta.quiltmc.org/v3/versions/loader/{mc}");
    let data: Vec<serde_json::Value> = get_json(client, &url).await?;
    Ok(data
        .iter()
        .filter_map(|v| {
            let loader = v.get("loader")?;
            Some(LoaderVersion {
                version: loader.get("version")?.as_str()?.to_string(),
                // Quilt 版本号含 beta 即为预览版
                stable: loader
                    .get("version")
                    .and_then(|s| s.as_str())
                    .map(|s| !s.contains("beta"))
                    .unwrap_or(true),
            })
        })
        .collect())
}

/// Forge 全部版本（从 maven-metadata.xml 解析，最新在前）。
pub async fn list_forge(client: &reqwest::Client) -> Result<Vec<String>> {
    let url = "https://maven.minecraftforge.net/net/minecraftforge/forge/maven-metadata.xml";
    let text = get_text(client, url).await?;
    Ok(parse_maven_versions(&text))
}

/// NeoForge 全部版本（最新在前）。
pub async fn list_neoforge(client: &reqwest::Client) -> Result<Vec<String>> {
    let url = "https://maven.neoforged.net/releases/net/neoforged/neoforge/maven-metadata.xml";
    let text = get_text(client, url).await?;
    Ok(parse_maven_versions(&text))
}

/// 某 MC 版本可用的 OptiFine 列表（BMCLAPI）。
pub async fn list_optifine(client: &reqwest::Client, mc: &str) -> Result<Vec<OptiFineEntry>> {
    let url = format!("https://bmclapi2.bangbang93.com/optifine/{mc}");
    let data: Vec<OptiFineEntry> = get_json(client, &url).await?;
    Ok(data)
}

/// 从 Forge 版本号中提取 MC 版本：`1.20.1-47.2.0` -> `1.20.1`。
pub fn forge_mc_version(forge_version: &str) -> &str {
    forge_version.split('-').next().unwrap_or(forge_version)
}

/// 过滤出指定 MC 版本的 Forge 版本。
pub fn filter_forge(all: &[String], mc: &str) -> Vec<String> {
    all.iter()
        .filter(|v| v.starts_with(&format!("{mc}-")))
        .cloned()
        .collect()
}

/// NeoForge 版本号与 MC 版本的对应（20.2+ 起版本号前两位对应 MC 次版本）。
/// 这里用 maven-metadata 无法直接拿到 MC 版本，因此安装时由用户选择的
/// NeoForge 版本与 MC 版本的匹配在 GUI 层做提示。
pub fn neoforge_mc_hint(neoforge_version: &str) -> String {
    // 21.1.x -> 1.21.1，20.4.x -> 1.20.4
    let parts: Vec<&str> = neoforge_version.split('.').collect();
    if parts.len() >= 2
        && let (Ok(a), Ok(b)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>())
    {
        format!("1.{a}.{b}")
    } else {
        String::new()
    }
}

/* ----------------------------- 安装：Fabric / Quilt ----------------------------- */

/// 安装 Fabric：写入 profile JSON，返回新的版本 id。
pub async fn install_fabric(
    client: &reqwest::Client,
    paths: &LauncherPaths,
    mc: &str,
    loader: &str,
    tx: &mpsc::UnboundedSender<InstallMessage>,
) -> Result<String> {
    let _ = tx.send(InstallMessage::Status(format!(
        "正在获取 Fabric {loader} for {mc}…"
    )));
    let url = format!(
        "https://meta.fabricmc.net/v2/versions/loader/{mc}/{loader}/profile/json"
    );
    let profile = get_text(client, &url).await?;
    let id = save_profile(paths, &profile)?;
    let _ = tx.send(InstallMessage::Status(format!("Fabric 已安装：{id}")));
    Ok(id)
}

/// 安装 Quilt：写入 profile JSON，返回新的版本 id。
pub async fn install_quilt(
    client: &reqwest::Client,
    paths: &LauncherPaths,
    mc: &str,
    loader: &str,
    tx: &mpsc::UnboundedSender<InstallMessage>,
) -> Result<String> {
    let _ = tx.send(InstallMessage::Status(format!(
        "正在获取 Quilt {loader} for {mc}…"
    )));
    let url = format!(
        "https://meta.quiltmc.org/v3/versions/loader/{mc}/{loader}/profile/json"
    );
    let profile = get_text(client, &url).await?;
    let id = save_profile(paths, &profile)?;
    let _ = tx.send(InstallMessage::Status(format!("Quilt 已安装：{id}")));
    Ok(id)
}

/// 解析 profile JSON 中的 id 并写入 `versions/<id>/<id>.json`。
fn save_profile(paths: &LauncherPaths, profile_text: &str) -> Result<String> {
    let value: serde_json::Value =
        serde_json::from_str(profile_text).context("解析加载器 profile JSON 失败")?;
    let id = value
        .get("id")
        .and_then(|v| v.as_str())
        .context("profile JSON 缺少 id 字段")?
        .to_string();
    let dir = paths.version_dir(&id);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(paths.version_json(&id), profile_text)?;
    Ok(id)
}

/* ----------------------------- 安装：Forge / NeoForge ----------------------------- */

/// 安装 Forge：下载安装器并运行 `--installClient`。
pub async fn install_forge(
    client: &reqwest::Client,
    paths: &LauncherPaths,
    forge_version: &str,
    threads: usize,
    java_override: Option<&Path>,
    tx: &mpsc::UnboundedSender<InstallMessage>,
) -> Result<String> {
    let mc = forge_mc_version(forge_version);
    let url = format!(
        "https://maven.minecraftforge.net/net/minecraftforge/forge/{v}/forge-{v}-installer.jar",
        v = forge_version
    );
    let installer = download_installer(
        client,
        paths,
        &url,
        format!("forge-{forge_version}-installer.jar"),
        threads,
        tx,
    )
    .await?;

    // 运行安装器需要的 Java：1.17+ 用 Java 17，否则 Java 8
    let java = pick_installer_java(java_override, mc, 17)?;

    // 新式（1.13+）安装器走原生 processor 编排，避免官方黑盒的 launcher_profiles
    // 检查、Java 版本挑剔与 GUI 问题；旧式安装器回退官方静默安装。
    if crate::core::installer::is_new_installer(&installer) {
        let id = crate::core::installer::install(
            client, paths, &installer, mc, &java, threads, tx,
        )
        .await?;
        let _ = tx.send(InstallMessage::Status(format!(
            "Forge {forge_version} 安装完成（版本 {id}，MC {mc}）"
        )));
        return Ok(id);
    }

    let _ = tx.send(InstallMessage::Status(
        "旧式安装器：正在运行 Forge 官方安装器（静默安装）…".to_string(),
    ));
    run_installer(&java, &installer, paths, &["--installClient"], tx).await?;
    // 旧式官方安装器生成的版本 id 为 `<mc>-forge-<v>`
    let id = format!("{mc}-forge-{forge_version}");
    let _ = tx.send(InstallMessage::Status(format!(
        "Forge {forge_version} 安装完成（版本 {id}，MC {mc}）"
    )));
    Ok(id)
}

/// 安装 NeoForge：下载安装器并运行 `--installClient`。
pub async fn install_neoforge(
    client: &reqwest::Client,
    paths: &LauncherPaths,
    neoforge_version: &str,
    threads: usize,
    java_override: Option<&Path>,
    tx: &mpsc::UnboundedSender<InstallMessage>,
) -> Result<String> {
    let url = format!(
        "https://maven.neoforged.net/releases/net/neoforged/neoforge/{v}/neoforge-{v}-installer.jar",
        v = neoforge_version
    );
    let installer = download_installer(
        client,
        paths,
        &url,
        format!("neoforge-{neoforge_version}-installer.jar"),
        threads,
        tx,
    )
    .await?;

    // NeoForge 全系列为较新版本，推荐 Java 17（旧 20.x）/ 21（新）；这里默认按 21
    let java = pick_installer_java(java_override, "", 21)?;

    // 新式安装器走原生 processor 编排（NeoForge 与 Forge 同构）
    if crate::core::installer::is_new_installer(&installer) {
        let id = crate::core::installer::install(
            client, paths, &installer, "", &java, threads, tx,
        )
        .await?;
        let _ = tx.send(InstallMessage::Status(format!(
            "NeoForge {neoforge_version} 安装完成（版本 {id}）"
        )));
        return Ok(id);
    }

    let _ = tx.send(InstallMessage::Status(
        "旧式安装器：正在运行 NeoForge 官方安装器（静默安装）…".to_string(),
    ));
    run_installer(&java, &installer, paths, &["--installClient"], tx).await?;
    let id = format!("neoforge-{neoforge_version}");
    let _ = tx.send(InstallMessage::Status(format!(
        "NeoForge {neoforge_version} 安装完成（版本 {id}）"
    )));
    Ok(id)
}

/* ----------------------------- 安装：OptiFine ----------------------------- */

/// 安装 OptiFine：下载安装器后启动官方安装器。
///
/// OptiFine 官方未提供静默安装参数，会弹出官方安装窗口，用户点击 `Install`
/// 即可（安装目标自动识别为当前游戏目录）。
pub async fn install_optifine(
    client: &reqwest::Client,
    paths: &LauncherPaths,
    entry: &OptiFineEntry,
    threads: usize,
    java_override: Option<&Path>,
    tx: &mpsc::UnboundedSender<InstallMessage>,
) -> Result<()> {
    // 优先 BMCLAPI，失败再尝试官网
    let bmcl_url = format!(
        "https://bmclapi2.bangbang93.com/optifine/{}/{}/{}",
        entry.mcversion, entry.of_type, entry.patch
    );
    let official_url =
        format!("https://optifine.net/download?f={}", entry.filename);

    let _ = tx.send(InstallMessage::Status(format!(
        "正在下载 OptiFine 安装器：{}…",
        entry.filename
    )));
    let target = installers_cache_dir(paths).join(&entry.filename);
    let task = DownloadTask {
        url: bmcl_url.clone(),
        target: target.clone(),
        sha1: None,
        size: None,
    };
    if let Err(e) =
        run_single_download(client.clone(), vec![task], threads, tx).await
    {
        let _ = tx.send(InstallMessage::Status(format!(
            "BMCLAPI 下载失败（{e}），尝试官网…"
        )));
        let task = DownloadTask {
            url: official_url,
            target: target.clone(),
            sha1: None,
            size: None,
        };
        run_single_download(client.clone(), vec![task], threads, tx).await?;
    }

    // OptiFine 安装器：1.17+ 用 Java 17，否则 Java 8
    let java = pick_installer_java(java_override, &entry.mcversion, 17)?;
    // 官方安装器同样要求游戏目录已初始化
    ensure_launcher_profiles(paths)?;
    let _ = tx.send(InstallMessage::Status(
        "正在启动 OptiFine 官方安装器，请在弹出的窗口中点击 Install…".to_string(),
    ));

    // 官方安装器主类为 optifine.Installer，工作目录设为游戏目录
    let status = Command::new(&java.path)
        .arg("-cp")
        .arg(&target)
        .arg("optifine.Installer")
        .current_dir(&paths.base_dir)
        .status()
        .await;
    match status {
        Ok(s) if s.success() => {
            let _ = tx.send(InstallMessage::Status(
                "OptiFine 安装完成".to_string(),
            ));
            Ok(())
        }
        Ok(s) => bail!("OptiFine 安装器异常退出（退出码 {:?}）", s.code()),
        Err(e) => bail!("无法启动 OptiFine 安装器：{e}"),
    }
}

/* ----------------------------- 公共辅助 ----------------------------- */

/// 安装器缓存目录（游戏目录下 `.installers`）。
fn installers_cache_dir(paths: &LauncherPaths) -> PathBuf {
    paths.base_dir.join(".installers")
}

/// Forge / NeoForge / OptiFine 官方安装器会检查游戏目录下的
/// `launcher_profiles.json`（官方启动器首次运行时生成），缺失就会直接报错：
/// “There is no Minecraft launcher profile ... you need to run the launcher first!”。
///
/// 本启动器不依赖该文件，但为了让官方安装器能正常工作，这里在目录缺失该文件时
/// 补写一个最小但合法的版本。
pub fn ensure_launcher_profiles(paths: &LauncherPaths) -> Result<()> {
    let _ = paths.ensure_dirs();
    std::fs::create_dir_all(&paths.base_dir)?;
    let file = paths.base_dir.join("launcher_profiles.json");
    if file.exists() {
        return Ok(());
    }
    let json = serde_json::json!({
        "profiles": {},
        "settings": {
            "crashAssertsEnabled": false,
            "enableAdvanced": false,
            "hasShownAdvanced": false,
            "keepLauncherOpen": false,
            "showMenu": false,
            "showSnapshots": false
        },
        "version": 3
    });
    std::fs::write(&file, serde_json::to_vec_pretty(&json)?)
        .with_context(|| format!("无法写入 {}", file.display()))?;
    Ok(())
}

/// 下载安装器到缓存目录并返回其路径。
async fn download_installer(
    client: &reqwest::Client,
    paths: &LauncherPaths,
    url: &str,
    filename: String,
    threads: usize,
    tx: &mpsc::UnboundedSender<InstallMessage>,
) -> Result<PathBuf> {
    let target = installers_cache_dir(paths).join(filename);
    let task = DownloadTask {
        url: url.to_string(),
        target: target.clone(),
        sha1: None,
        size: None,
    };
    run_single_download(client.clone(), vec![task], threads, tx).await?;
    Ok(target)
}

/// 以静默方式运行安装器（Forge / NeoForge）。
async fn run_installer(
    java: &JavaInfo,
    installer: &Path,
    paths: &LauncherPaths,
    extra_args: &[&str],
    tx: &mpsc::UnboundedSender<InstallMessage>,
) -> Result<()> {
    // 官方安装器要求游戏目录已初始化（含 launcher_profiles.json）
    ensure_launcher_profiles(paths)?;

    let mut cmd = Command::new(&java.path);
    cmd.arg("-jar")
        .arg(installer)
        .args(extra_args)
        .arg(&paths.base_dir)
        .current_dir(&paths.base_dir);

    let output = cmd
        .output()
        .await
        .context("无法运行安装器（检查 Java）")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut msg = format!(
            "安装器执行失败（退出码 {}）：\n{}\n{}",
            output.status.code().unwrap_or(-1),
            stderr.trim(),
            stdout.trim()
        );
        // 常见错误给出更明确的中文提示
        let combined = format!("{stderr} {stdout}").to_lowercase();
        if combined.contains("launcher profile") {
            msg.push_str(
                "\n\n提示：安装器要求游戏目录已初始化（launcher_profiles.json）。\
                 启动器已尝试自动补建，若仍失败，请确认游戏目录可写后重试。",
            );
        }
        if combined.contains("unsupportedclassversion")
            || combined.contains("class file version")
            || combined.contains("has been compiled by a more recent")
        {
            msg.push_str(
                "\n\n提示：当前 Java 版本与安装器不匹配。请在「设置」里把自定义 Java \
                 指向合适版本（1.16 及更早用 Java 8；1.17~1.20.4 用 Java 17；更新版本用 Java 21）。",
            );
        }
        bail!("{msg}");
    }
    let _ = tx.send(InstallMessage::Status(
        "安装器执行完毕，正在校验结果…".to_string(),
    ));
    Ok(())
}

/// 选择运行安装器的 Java。
///
/// `java_override` 为用户配置的自定义 Java；否则自动检测。
/// 优先精确匹配该 MC 版本推荐的 LTS 版本（8 / 17 / 21），避免误用过高的
/// 早期访问版本（如 Java 25）；只有找不到时才回退到更高版本。
fn pick_installer_java(
    java_override: Option<&Path>,
    mc_version: &str,
    fallback_min: u32,
) -> Result<JavaInfo> {
    if let Some(p) = java_override {
        if !p.as_os_str().is_empty() {
            return java::probe(p).context("自定义 Java 不可用");
        }
    }
    let recommended = if mc_version.is_empty() {
        fallback_min
    } else {
        mc_recommended_java(mc_version)
    };

    let available = java::detect_all();
    if available.is_empty() {
        bail!("未找到 Java，请在「设置」里指定 Java 可执行文件路径");
    }

    // 1. 精确匹配推荐的 LTS 版本
    if let Some(j) = available.iter().find(|j| j.major == recommended) {
        return Ok(j.clone());
    }
    // 2. 在 [推荐, 21] 这个 LTS 区间内选最低的
    let mut in_range: Vec<&JavaInfo> = available
        .iter()
        .filter(|j| j.major >= recommended && j.major <= 21)
        .collect();
    in_range.sort_by_key(|j| j.major);
    if let Some(j) = in_range.first() {
        return Ok((*j).clone());
    }
    // 3. 高于推荐的最低版本（可能是较新的 Java，如 25）
    let mut higher: Vec<&JavaInfo> = available
        .iter()
        .filter(|j| j.major >= recommended)
        .collect();
    higher.sort_by_key(|j| j.major);
    if let Some(j) = higher.first() {
        return Ok((*j).clone());
    }
    // 4. 兜底：选最高版本（可能过旧，安装器会在运行时报错）
    available
        .iter()
        .max_by_key(|j| j.major)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("未找到可运行安装器的 Java（推荐 Java {recommended}）"))
}

/// 根据 MC 版本推荐 Java 主版本。
///
/// - 1.16 及更早 → Java 8；
/// - 1.17 ～ 1.20.4 → Java 17；
/// - 1.20.5 / 1.21 及更新（含新版本号如 26.x）→ Java 21。
pub fn mc_recommended_java(mc: &str) -> u32 {
    if let Some(rest) = mc.strip_prefix("1.") {
        let mut it = rest.split('.');
        let a: u32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let b: u32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        if a <= 16 {
            8
        } else if a < 20 {
            17
        } else if a == 20 && b < 5 {
            17
        } else {
            21
        }
    } else {
        // 非 1.x 命名的新版本一律按最新 LTS
        21
    }
}

/// 运行单文件下载（把下载事件转发为 InstallMessage）。
async fn run_single_download(
    client: reqwest::Client,
    tasks: Vec<DownloadTask>,
    threads: usize,
    tx: &mpsc::UnboundedSender<InstallMessage>,
) -> Result<()> {
    let (dl_tx, mut dl_rx) = mpsc::unbounded_channel::<DownloadEvent>();
    let fwd = tx.clone();
    let forwarder = tokio::spawn(async move {
        while let Some(e) = dl_rx.recv().await {
            let _ = fwd.send(InstallMessage::Download(e));
        }
    });
    let result = download::run(client, tasks, dl_tx, threads.max(1)).await;
    forwarder.abort();
    let _ = dl_rx;
    result
}

/* ----------------------------- 网络与解析 ----------------------------- */

async fn get_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
) -> Result<T> {
    let text = get_text(client, url).await?;
    serde_json::from_str(&text).with_context(|| format!("解析 JSON 失败：{url}"))
}

/// 发起 GET 请求并读取文本。对网络中断 / TLS 中断 / 超时等可恢复错误自动重试，
/// 以兼容不稳定的网络与代理环境。
pub(crate) async fn get_text(client: &reqwest::Client, url: &str) -> Result<String> {
    let max_attempts = 4u32;
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 0..max_attempts {
        if attempt > 0 {
            // 指数退避：0.4s、0.9s、1.6s
            tokio::time::sleep(std::time::Duration::from_millis((400 + attempt * 250) as u64)).await;
        }
        let result = async {
            let text = client
                .get(url)
                .send()
                .await
                .with_context(|| format!("请求失败：{url}"))?
                .error_for_status()?
                .text()
                .await
                .with_context(|| format!("读取内容失败：{url}"))?;
            Ok::<String, anyhow::Error>(text)
        }
        .await;
        match result {
            Ok(t) => return Ok(t),
            Err(e) => {
                // 4xx（除 408/429）不重试；连接 / TLS / 超时类错误重试。
                let retryable = is_retryable_error(&e);
                last_err = Some(e);
                if !retryable {
                    break;
                }
            }
        }
    }
    Err(last_err
        .unwrap_or_else(|| anyhow::anyhow!("请求失败（重试耗尽）：{url}")))
}

/// 判断错误是否值得重试（连接重置、TLS 中断、超时、429/5xx）。
fn is_retryable_error(e: &anyhow::Error) -> bool {
    // 检查 reqwest 状态码
    for cause in e.chain() {
        if let Some(status) = cause.downcast_ref::<reqwest::Error>()
            && let Some(code) = status.status()
        {
            if code == reqwest::StatusCode::TOO_MANY_REQUESTS
                || code.is_server_error()
                || code == reqwest::StatusCode::REQUEST_TIMEOUT
            {
                return true;
            }
            // 其它明确的 HTTP 状态码错误（4xx）不重试
            return false;
        }
    }
    // 连接 / TLS / 超时类错误：reqwest::Error 的 is_connect/is_timeout/is_request
    for cause in e.chain() {
        if let Some(re) = cause.downcast_ref::<reqwest::Error>() {
            if re.is_connect() || re.is_timeout() || re.is_request() || re.is_body() {
                return true;
            }
        }
    }
    // 兜底：错误信息里含典型的中断关键字
    let msg = format!("{e:#}").to_lowercase();
    msg.contains("close_notify")
        || msg.contains("connection")
        || msg.contains("timed out")
        || msg.contains("reset")
        || msg.contains("tls")
        || msg.contains("eof")
}

/// 带重试地执行一个 GET 请求并反序列化 JSON。
/// `build` 闭包在每次尝试时重新构造请求（`RequestBuilder` 是一次性的）。
pub(crate) async fn fetch_json_retry<T, F>(build: F, label: &str) -> Result<T>
where
    T: serde::de::DeserializeOwned,
    F: Fn() -> reqwest::RequestBuilder,
{
    let max_attempts = 4u32;
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 0..max_attempts {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_millis((400 + attempt * 250) as u64)).await;
        }
        let result = async {
            let text = build()
                .send()
                .await
                .with_context(|| format!("请求失败：{label}"))?
                .error_for_status()?
                .text()
                .await
                .with_context(|| format!("读取内容失败：{label}"))?;
            Ok::<String, anyhow::Error>(text)
        }
        .await;
        match result {
            Ok(text) => {
                return serde_json::from_str(&text)
                    .with_context(|| format!("解析 JSON 失败：{label}"));
            }
            Err(e) => {
                let retryable = is_retryable_error(&e);
                last_err = Some(e);
                if !retryable {
                    break;
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("请求失败（重试耗尽）：{label}")))
}

/// 解析 maven-metadata.xml 中的 `<version>` 列表并倒序（最新在前）。
fn parse_maven_versions(xml: &str) -> Vec<String> {
    let mut versions = Vec::new();
    for line in xml.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("<version>")
            && let Some(v) = rest.strip_suffix("</version>")
        {
            versions.push(v.to_string());
        }
    }
    // maven-metadata 通常按时间正序，倒序后最新在前
    versions.reverse();
    versions
}
