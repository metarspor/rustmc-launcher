//! 原生 Forge / NeoForge 安装器（参考 HMCL 的 processor 编排机制）。
//!
//! 官方安装器（`--installClient`）是一个黑盒：它要求游戏目录里存在
//! `launcher_profiles.json`、对 Java 版本挑剔、且不提供细粒度进度。
//!
//! 1.13 及更新的 Forge / NeoForge 安装器本身就是一个“安装包 + 一串 Java 处理器”：
//! - `install_profile.json` 描述处理器（processors）、数据（data）、依赖库；
//! - `version.json` 是加载器的版本清单（`inheritsFrom` 原版）；
//! - `maven/` 目录内置了部分依赖库（有些版本为空，需在线下载）。
//!
//! 本模块按 HMCL 的做法自己编排：
//! 1. 解析 `install_profile.json`；
//! 2. 复制 `maven/` 内置库、下载缺失的处理器与运行库；
//! 3. 写入加载器版本 JSON；
//! 4. 准备数据变量，依次运行客户端处理器（反混淆、拆包、打二进制补丁等）；
//! 5. 校验处理器产出物。
//!
//! 全程不依赖官方安装器的 GUI，也不依赖 `launcher_profiles.json`。
//!
//! 1.12 及更早的旧式安装器（`install` + `versionInfo` 结构）不在此列，
//! 由调用方回退到官方静默安装。
#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail};
use futures::StreamExt;
use sha1::{Digest, Sha1};
use tokio::process::Command;
use tokio::sync::{mpsc, Semaphore};
use zip::ZipArchive;

use crate::core::download::DownloadEvent;
use crate::core::java::JavaInfo;
use crate::core::manifest::{VersionDetail, VersionManifest};
use crate::core::modloaders::InstallMessage;
use crate::core::paths::LauncherPaths;

/* --------------------------- install_profile 数据结构 --------------------------- */

#[derive(Debug, serde::Deserialize)]
struct RawProfile {
    /// 新版安装器为 1；旧式安装器无此字段
    spec: Option<i32>,
    minecraft: Option<String>,
    /// 加载器版本 JSON 在安装器内的路径，如 `/version.json`
    json: Option<String>,
    /// 加载器版本 id，如 `1.20.1-forge-47.2.0`
    version: Option<String>,
    /// 旧式安装器的“安装信息”块
    install: Option<serde_json::Value>,
    /// 旧式安装器的版本信息块
    #[serde(rename = "versionInfo")]
    version_info: Option<serde_json::Value>,
    #[serde(default)]
    libraries: Vec<RawLib>,
    #[serde(default)]
    processors: Vec<RawProcessor>,
    #[serde(default)]
    data: HashMap<String, RawDatum>,
    profile: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct RawDatum {
    client: String,
    #[serde(default)]
    server: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct RawProcessor {
    sides: Option<Vec<String>>,
    jar: String,
    #[serde(default)]
    classpath: Vec<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    outputs: HashMap<String, String>,
}

#[derive(Debug, serde::Deserialize)]
struct RawLib {
    name: String,
    downloads: Option<RawLibDownloads>,
    url: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct RawLibDownloads {
    artifact: Option<RawArtifact>,
}

#[derive(Debug, serde::Deserialize, Clone)]
struct RawArtifact {
    path: Option<String>,
    url: Option<String>,
    sha1: Option<String>,
    size: Option<u64>,
}

/* ------------------------------ maven 坐标 ------------------------------ */

/// 一个 maven 坐标：`group:name:version[:classifier][@ext]`。
struct Coord {
    rel_path: String,
}

/// 解析坐标并计算相对仓库根的路径。
fn parse_coord(descriptor: &str) -> Result<Coord> {
    let (coord, ext) = match descriptor.rsplit_once('@') {
        Some((c, e)) => (c, e),
        None => (descriptor, "jar"),
    };
    let parts: Vec<&str> = coord.split(':').collect();
    if parts.len() < 3 || parts.len() > 4 {
        bail!("无法解析的 maven 坐标：{descriptor}");
    }
    let group = parts[0].replace('.', "/");
    let name = parts[1];
    let version = parts[2];
    let classifier = parts.get(3).copied();
    let mut file = format!("{name}-{version}");
    if let Some(c) = classifier {
        file.push('-');
        file.push_str(c);
    }
    file.push('.');
    file.push_str(ext);
    Ok(Coord {
        rel_path: format!("{group}/{name}/{version}/{file}"),
    })
}

/* ------------------------------ zip 读取 ------------------------------ */

/// 读取安装器内某个文件的内容，不存在时返回 `None`。
fn zip_read(installer: &Path, name: &str) -> Result<Option<Vec<u8>>> {
    let file = std::fs::File::open(installer)
        .with_context(|| format!("打开安装器失败：{}", installer.display()))?;
    let mut zip = ZipArchive::new(file).context("安装器不是合法的 zip/jar")?;
    let mut entry = match zip.by_name(name) {
        Ok(e) => e,
        Err(_) => return Ok(None),
    };
    let mut buf = Vec::new();
    entry.read_to_end(&mut buf)?;
    Ok(Some(buf))
}

/// 把安装器内 `maven/` 下的所有文件原样复制到 libraries 目录。
fn copy_bundled_maven(installer: &Path, libraries: &Path) -> Result<u64> {
    let file = std::fs::File::open(installer)?;
    let mut zip = ZipArchive::new(file)?;
    let mut count = 0u64;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let name = entry.name()?.to_string();
        if !name.starts_with("maven/") || name.ends_with('/') {
            continue;
        }
        let rel = &name["maven/".len()..];
        if rel.is_empty() {
            continue;
        }
        let dest = libraries.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = std::fs::File::create(&dest)?;
        std::io::copy(&mut entry, &mut out)?;
        count += 1;
    }
    Ok(count)
}

/// 读取 jar 中 `META-INF/MANIFEST.MF` 的 `Main-Class`。
fn jar_main_class(jar: &Path) -> Result<String> {
    let bytes = zip_read(jar, "META-INF/MANIFEST.MF")?
        .with_context(|| format!("处理器 jar 缺少清单：{}", jar.display()))?;
    let text = String::from_utf8_lossy(&bytes);
    for line in text.lines() {
        if let Some(v) = line.trim().strip_prefix("Main-Class:") {
            return Ok(v.trim().to_string());
        }
    }
    bail!("处理器 jar 没有 Main-Class：{}", jar.display());
}

/* ------------------------------ 变量替换 ------------------------------ */

/// 取被一对前后缀完整包裹时的内部内容。
fn surrounded<'a>(s: &'a str, open: char, close: char) -> Option<&'a str> {
    let t = s.trim();
    let mut chars = t.chars();
    if chars.next() != Some(open) {
        return None;
    }
    if chars.next_back() != Some(close) {
        return None;
    }
    Some(&t[open.len_utf8()..t.len() - close.len_utf8()])
}

/// 把字符串中的 `{VAR}` 替换为变量值；`'literal'` 去掉引号作为字面量。
fn replace_tokens(vars: &HashMap<String, String>, input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '{' => {
                if let Some(end) = chars[i + 1..].iter().position(|&c| c == '}') {
                    let key: String = chars[i + 1..i + 1 + end].iter().collect();
                    if let Some(v) = vars.get(&key) {
                        out.push_str(v);
                    }
                    i = i + 1 + end + 1;
                } else {
                    out.push('{');
                    i += 1;
                }
            }
            '\'' => {
                if let Some(end) = chars[i + 1..].iter().position(|&c| c == '\'') {
                    let literal: String = chars[i + 1..i + 1 + end].iter().collect();
                    out.push_str(&literal);
                    i = i + 1 + end + 1;
                } else {
                    out.push('\'');
                    i += 1;
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// 解析一个字面量（data 值或 processor 参数）。
///
/// - `{var}`：取变量；
/// - `'literal'`：字面量（去掉引号）；
/// - `[group:name:version:classifier@ext]`：解析为 libraries 中的文件路径；
/// - 其他：对其中的 `{var}` 做替换。
fn parse_literal(
    literal: &str,
    vars: &HashMap<String, String>,
    libraries: &Path,
) -> Result<String> {
    if let Some(k) = surrounded(literal, '{', '}') {
        return vars
            .get(k)
            .cloned()
            .with_context(|| format!("安装变量 `{k}` 未定义"));
    }
    if let Some(inner) = surrounded(literal, '\'', '\'') {
        return Ok(inner.to_string());
    }
    if let Some(inner) = surrounded(literal, '[', ']') {
        return Ok(libraries.join(parse_coord(inner)?.rel_path).display().to_string());
    }
    Ok(replace_tokens(vars, literal))
}

/* ------------------------------ 入口 ------------------------------ */

/// 判断安装器是否为新式（带 spec 的 processor 结构）。
pub fn is_new_installer(installer: &Path) -> bool {
    let bytes = match zip_read(installer, "install_profile.json") {
        Ok(Some(b)) => b,
        _ => return false,
    };
    serde_json::from_slice::<serde_json::Value>(&bytes)
        .map(|v| v.get("spec").is_some())
        .unwrap_or(false)
}

/// 执行新式（processor 机制）安装。调用前已确认该安装器为新式。
///
/// 返回加载器版本 id（即 `versions/<id>/<id>.json`）。
pub async fn install(
    client: &reqwest::Client,
    paths: &LauncherPaths,
    installer: &Path,
    mc_hint: &str,
    java: &JavaInfo,
    threads: usize,
    tx: &mpsc::UnboundedSender<InstallMessage>,
) -> Result<String> {
    let _ = paths.ensure_dirs();
    let installer = installer.to_path_buf();
    let libraries_dir = paths.libraries_dir();

    // 1. 读取安装配置
    let profile_bytes = zip_read(&installer, "install_profile.json")?
        .context("安装器缺少 install_profile.json")?;
    let profile: RawProfile =
        serde_json::from_slice(&profile_bytes).context("解析 install_profile.json 失败")?;
    if profile.spec.is_none() {
        bail!("该安装器不是新式（processor）结构，应回退官方静默安装");
    }
    let mc = profile.minecraft.clone().unwrap_or_else(|| mc_hint.to_string());
    let version_id = profile
        .version
        .clone()
        .with_context(|| "install_profile.json 缺少 version 字段")?;
    let json_path = profile
        .json
        .clone()
        .unwrap_or_else(|| "/version.json".to_string());
    let json_name = json_path.trim_start_matches('/');

    let _ = tx.send(InstallMessage::Status(
        "正在读取安装器内置库与版本清单…".to_string(),
    ));

    // 2. 复制内置 maven 库
    let bundled = copy_bundled_maven(&installer, &libraries_dir)?;
    if bundled > 0 {
        let _ = tx.send(InstallMessage::Status(format!(
            "已复制安装器内置库 {bundled} 个"
        )));
    }

    // 3. 下载处理器依赖库（install_profile.libraries）
    let _ = tx.send(InstallMessage::Status(
        "正在下载安装处理器依赖库…".to_string(),
    ));
    let mut libs: Vec<LibSpec> = Vec::new();
    for lib in &profile.libraries {
        libs.push(lib_from_install(&lib.name, lib.downloads.as_ref(), lib.url.as_deref(), paths)?);
    }
    download_libraries(client, libs, threads, tx).await?;

    // 4. 读取并写入加载器版本 JSON，同时下载运行库
    let version_json_bytes = zip_read(&installer, json_name)?
        .with_context(|| format!("安装器缺少版本清单 {json_name}"))?;
    let loader_detail: VersionDetail =
        serde_json::from_slice(&version_json_bytes).context("解析加载器版本 JSON 失败")?;
    // 版本 id 以 version.json 的 id 为准，目录名保持一致
    let loader_id = loader_detail.id.clone();
    let version_dir = paths.version_dir(&loader_id);
    std::fs::create_dir_all(&version_dir)?;
    std::fs::write(paths.version_json(&loader_id), &version_json_bytes)?;
    let _ = tx.send(InstallMessage::Status(format!(
        "已写入版本清单 {loader_id}，正在下载运行库…"
    )));

    // 运行库（version.json 的 libraries，natives 由启动时常规流程处理）
    let mut run_libs: Vec<LibSpec> = Vec::new();
    for lib in loader_detail.libraries.iter().flatten() {
        let dl = lib.downloads.as_ref().map(|d| RawLibDownloads {
            artifact: d.artifact.as_ref().map(|a| RawArtifact {
                path: a.path.clone(),
                url: a.url.clone(),
                sha1: a.sha1.clone(),
                size: a.size,
            }),
        });
        run_libs.push(lib_from_install(
            &lib.name,
            dl.as_ref(),
            lib.url.as_deref(),
            paths,
        )?);
    }
    download_libraries(client, run_libs, threads, tx).await?;

    // 5. 确保原版客户端 jar 存在（处理器需要它）
    let _ = tx.send(InstallMessage::Status(format!(
        "正在准备原版客户端 {mc}…"
    )));
    let vanilla_jar = ensure_vanilla_client(client, paths, &mc, tx).await?;

    // 6. 准备临时目录与数据变量
    let temp_dir = std::env::temp_dir().join(format!("rustmc-forge-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp_dir);
    std::fs::create_dir_all(&temp_dir)?;
    let isolated_jar = temp_dir.join("minecraft.jar");
    std::fs::copy(&vanilla_jar, &isolated_jar)?;

    let mut vars = HashMap::new();
    prepare_data(&profile.data, &installer, &temp_dir, &libraries_dir, &mut vars)?;
    vars.insert("SIDE".to_string(), "client".to_string());
    vars.insert(
        "MINECRAFT_JAR".to_string(),
        absolute(&isolated_jar)?.display().to_string(),
    );
    vars.insert("MINECRAFT_VERSION".to_string(), mc.clone());
    vars.insert("ROOT".to_string(), absolute(&paths.base_dir)?.display().to_string());
    vars.insert(
        "INSTALLER".to_string(),
        absolute(&installer)?.display().to_string(),
    );
    vars.insert(
        "LIBRARY_DIR".to_string(),
        absolute(&libraries_dir)?.display().to_string(),
    );

    // 7. 依次运行客户端处理器
    let client_procs: Vec<&RawProcessor> = profile
        .processors
        .iter()
        .filter(|p| match &p.sides {
            None => true,
            Some(sides) => sides.iter().any(|s| s == "client"),
        })
        .collect();
    let total = client_procs.len();
    for (idx, proc) in client_procs.iter().enumerate() {
        run_processor(
            proc,
            &vars,
            &libraries_dir,
            java,
            paths,
            &temp_dir,
            idx + 1,
            total,
            tx,
        )
        .await?;
    }

    // 8. 清理
    let _ = std::fs::remove_dir_all(&temp_dir);
    // version_id 仅用于返回，真正以版本 JSON 的 id 为准
    let _ = version_id;
    Ok(loader_id)
}

/* ------------------------------ 库下载 ------------------------------ */

struct LibSpec {
    name: String,
    target: PathBuf,
    rel: String,
    urls: Vec<String>,
    sha1: Option<String>,
    size: Option<u64>,
}

/// 把 install_profile / version.json 的库描述转换为下载规范。
fn lib_from_install(
    name: &str,
    downloads: Option<&RawLibDownloads>,
    url_base: Option<&str>,
    paths: &LauncherPaths,
) -> Result<LibSpec> {
    let artifact = downloads.and_then(|d| d.artifact.as_ref());
    let rel = artifact
        .as_ref()
        .and_then(|a| a.path.clone())
        .or_else(|| parse_coord(name).ok().map(|c| c.rel_path))
        .with_context(|| format!("无法确定库的路径：{name}"))?;
    let target = paths.libraries_dir().join(&rel);
    let abs_url = artifact.as_ref().and_then(|a| a.url.clone());
    let urls = candidate_urls(&rel, abs_url.as_deref(), url_base);
    Ok(LibSpec {
        name: name.to_string(),
        target,
        rel,
        urls,
        sha1: artifact.as_ref().and_then(|a| a.sha1.clone()),
        size: artifact.as_ref().and_then(|a| a.size),
    })
}

/// 为一个库构造候选下载地址（绝对地址优先，其次 Forge/NeoForge 官方、
/// BMCLAPI 镜像、Maven Central、Mojang 库仓库）。
fn candidate_urls(rel: &str, abs_url: Option<&str>, url_base: Option<&str>) -> Vec<String> {
    let mut urls: Vec<String> = Vec::new();
    if let Some(u) = abs_url {
        urls.push(u.to_string());
    }
    if let Some(b) = url_base {
        urls.push(format!("{}/{}", b.trim_end_matches('/'), rel));
    }
    if rel.starts_with("net/minecraftforge/") {
        urls.push(format!("https://maven.minecraftforge.net/{rel}"));
    }
    if rel.starts_with("net/neoforged/") {
        urls.push(format!("https://maven.neoforged.net/releases/{rel}"));
    }
    urls.push(format!("https://bmclapi2.bangbang93.com/maven/{rel}"));
    urls.push(format!("https://repo1.maven.org/maven2/{rel}"));
    urls.push(format!("https://libraries.minecraft.net/{rel}"));
    urls.dedup();
    urls
}

/// 并发下载库，每个库会依次尝试候选地址；已存在且校验通过的跳过。
async fn download_libraries(
    client: &reqwest::Client,
    libs: Vec<LibSpec>,
    threads: usize,
    tx: &mpsc::UnboundedSender<InstallMessage>,
) -> Result<()> {
    let pending: Vec<LibSpec> = libs
        .into_iter()
        .filter(|l| needs_download(l))
        .collect();
    if pending.is_empty() {
        return Ok(());
    }
    let total_files = pending.len() as u64;
    let total_bytes = pending.iter().filter_map(|l| l.size).sum();
    let _ = tx.send(InstallMessage::Download(DownloadEvent::Start {
        total_files,
        total_bytes,
    }))?;

    let semaphore = Arc::new(Semaphore::new(threads.max(1)));
    let done = Arc::new(AtomicU64::new(0));
    let got_bytes = Arc::new(AtomicU64::new(0));
    let results = futures::stream::iter(pending)
        .map(|lib| {
            let client = client.clone();
            let semaphore = semaphore.clone();
            let done = done.clone();
            let got_bytes = got_bytes.clone();
            let tx = tx.clone();
            async move {
                let _permit = semaphore.acquire_owned().await?;
                let len = download_one_with_candidates(&client, &lib).await?;
                let c = done.fetch_add(1, Ordering::SeqCst) + 1;
                let b = got_bytes.fetch_add(lib.size.unwrap_or(len), Ordering::SeqCst)
                    + lib.size.unwrap_or(len);
                let _ = tx.send(InstallMessage::Download(DownloadEvent::FileDone {
                    completed: c,
                    total: total_files,
                    finished_bytes: b,
                    total_bytes,
                    name: lib
                        .target
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("?")
                        .to_string(),
                }));
                Ok::<(), anyhow::Error>(())
            }
        })
        .buffer_unordered(threads.max(1));

    let outcomes: Vec<_> = results.collect().await;
    for o in outcomes {
        o?;
    }
    Ok(())
}

fn needs_download(lib: &LibSpec) -> bool {
    match std::fs::metadata(&lib.target) {
        Ok(meta) => {
            if let Some(size) = lib.size {
                size != meta.len()
            } else if let Some(sha) = &lib.sha1 {
                sha1_file(&lib.target).map(|s| &s != sha).unwrap_or(true)
            } else {
                // 大小哈希都未知：已存在则信任，避免重复下载
                false
            }
        }
        Err(_) => true,
    }
}

/// 对单个库依次尝试候选地址，成功后校验 SHA1。
async fn download_one_with_candidates(
    client: &reqwest::Client,
    lib: &LibSpec,
) -> Result<u64> {
    if let Some(parent) = lib.target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut last_err: Option<anyhow::Error> = None;
    for url in &lib.urls {
        match try_fetch(client, url, lib.sha1.as_deref()).await {
            Ok(bytes) => {
                let len = bytes.len() as u64;
                let tmp = lib.target.with_extension("part");
                {
                    let mut f = std::fs::File::create(&tmp)?;
                    f.write_all(&bytes)?;
                }
                std::fs::rename(&tmp, &lib.target)?;
                return Ok(len);
            }
            Err(e) => last_err = Some(e),
        }
    }
    bail!(
        "库 {} 下载失败，所有镜像均不可用：{:?}",
        lib.name,
        last_err.map(|e| e.to_string())
    )
}

async fn try_fetch(
    client: &reqwest::Client,
    url: &str,
    expected_sha1: Option<&str>,
) -> Result<Vec<u8>> {
    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("请求失败：{url}"))?
        .error_for_status()
        .with_context(|| format!("服务器返回错误：{url}"))?;
    let bytes = resp.bytes().await?.to_vec();
    if let Some(expected) = expected_sha1 {
        let mut hasher = Sha1::new();
        hasher.update(&bytes);
        let actual = hex::encode(hasher.finalize());
        if actual != expected {
            bail!("SHA1 校验失败：{url}");
        }
    }
    Ok(bytes)
}

/* ------------------------------ 原版客户端 ------------------------------ */

/// 确保原版 `versions/<mc>/<mc>.json` 与 `<mc>.jar` 就绪，返回 client.jar 路径。
async fn ensure_vanilla_client(
    client: &reqwest::Client,
    paths: &LauncherPaths,
    mc: &str,
    tx: &mpsc::UnboundedSender<InstallMessage>,
) -> Result<PathBuf> {
    let json_path = paths.version_json(mc);
    if !json_path.exists() {
        let manifest_text = fetch_json_retry_text(
            client,
            "https://launchermeta.mojang.com/mc/game/version_manifest_v2.json",
        )
        .await
        .context("获取原版版本清单失败")?;
        let manifest: VersionManifest =
            serde_json::from_str(&manifest_text).context("解析原版版本清单失败")?;
        let entry = manifest
            .versions
            .iter()
            .find(|v| v.id == mc)
            .with_context(|| format!("原版版本清单中找不到 {mc}"))?;
        let text = fetch_json_retry_text(client, &entry.url).await?;
        std::fs::create_dir_all(paths.version_dir(mc))?;
        std::fs::write(&json_path, text)?;
    }
    let detail: VersionDetail =
        serde_json::from_slice(&std::fs::read(&json_path)?).context("解析原版版本 JSON 失败")?;
    let jar = paths.version_jar(mc);
    if !jar.exists() {
        let client_dl = detail
            .downloads
            .as_ref()
            .and_then(|d| d.get("client"))
            .with_context(|| format!("版本 {mc} 缺少 client 下载信息"))?;
        let _ = tx.send(InstallMessage::Status(format!(
            "正在下载原版客户端 {mc}.jar…"
        )));
        let bytes = fetch_bytes_retry(client, &client_dl.url, client_dl.sha1.as_deref()).await?;
        std::fs::write(&jar, bytes)?;
    }
    Ok(jar)
}

/// 带网络重试的字节获取（下载较大的原版客户端，规避偶发 TLS 中断）。
async fn fetch_bytes_retry(
    client: &reqwest::Client,
    url: &str,
    expected_sha1: Option<&str>,
) -> Result<Vec<u8>> {
    let mut last = None;
    for attempt in 0..4u32 {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(400 + attempt as u64 * 300))
                .await;
        }
        match try_fetch(client, url, expected_sha1).await {
            Ok(b) => return Ok(b),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("未知错误")))
}

/// 带网络重试的文本获取（复用 modloaders 的重试策略）。
async fn fetch_json_retry_text(client: &reqwest::Client, url: &str) -> Result<String> {
    // 通过 install 模块暴露的重试入口；这里直接做简单指数退避。
    let mut last = None;
    for attempt in 0..4u32 {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(400 + attempt as u64 * 250))
                .await;
        }
        match client.get(url).send().await {
            Ok(r) => match r.error_for_status() {
                Ok(r) => match r.text().await {
                    Ok(t) => return Ok(t),
                    Err(e) => last = Some(anyhow::anyhow!(e)),
                },
                Err(e) => last = Some(anyhow::anyhow!(e)),
            },
            Err(e) => last = Some(anyhow::anyhow!(e)),
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("未知错误")))
}

/* ------------------------------ 数据变量 ------------------------------ */

/// 解析 data 块：内置文件从安装器解压到临时目录；maven 产物解析为 libraries 路径；
/// 单引号包裹的作为字面量。
fn prepare_data(
    data: &HashMap<String, RawDatum>,
    installer: &Path,
    temp_dir: &Path,
    libraries: &Path,
    vars: &mut HashMap<String, String>,
) -> Result<()> {
    for (key, datum) in data {
        let value = &datum.client;
        let resolved = if let Some(coord) = surrounded(value, '[', ']') {
            libraries
                .join(parse_coord(coord)?.rel_path)
                .display()
                .to_string()
        } else if let Some(literal) = surrounded(value, '\'', '\'') {
            literal.to_string()
        } else if let Some(other) = surrounded(value, '{', '}') {
            vars.get(other).cloned().unwrap_or_default()
        } else {
            // 安装器内的文件（如 /data/client.lzma），解压到临时目录
            let zip_name = value.trim_start_matches('/');
            let bytes = zip_read(installer, zip_name)?
                .with_context(|| format!("安装器缺少数据文件 {zip_name}"))?;
            let flat = zip_name.replace('/', "_");
            let out = temp_dir.join(flat);
            let mut f = std::fs::File::create(&out)?;
            f.write_all(&bytes)?;
            out.display().to_string()
        };
        vars.insert(key.clone(), resolved);
    }
    Ok(())
}

/* ------------------------------ 处理器执行 ------------------------------ */

async fn run_processor(
    proc: &RawProcessor,
    vars: &HashMap<String, String>,
    libraries: &Path,
    java: &JavaInfo,
    paths: &LauncherPaths,
    _temp_dir: &Path,
    idx: usize,
    total: usize,
    tx: &mpsc::UnboundedSender<InstallMessage>,
) -> Result<()> {
    let proc_jar = libraries.join(parse_coord(&proc.jar)?.rel_path);
    if !proc_jar.is_file() {
        bail!(
            "处理器依赖缺失：{}（{}），请检查下载是否完整",
            proc.jar,
            proc_jar.display()
        );
    }

    // 先按 outputs 判断是否已完成（可重复安装时跳过）
    let mut miss = proc.outputs.is_empty();
    let mut resolved_outputs: Vec<(PathBuf, String)> = Vec::new();
    for (k, v) in &proc.outputs {
        let key = parse_literal(k, vars, libraries)?;
        let val = parse_literal(v, vars, libraries)?;
        resolved_outputs.push((PathBuf::from(&key), val.clone()));
        match sha1_file(&key) {
            Ok(actual) if actual == val => {}
            _ => {
                let _ = std::fs::remove_file(&key);
                miss = true;
            }
        }
    }
    if !miss {
        let _ = tx.send(InstallMessage::Status(format!(
            "处理器 {idx}/{total} 已完成，跳过（{}）",
            short_coord(&proc.jar)
        )));
        return Ok(());
    }

    let main_class = jar_main_class(&proc_jar)?;

    // classpath：处理器声明的 classpath + 自身
    let sep = if cfg!(windows) { ";" } else { ":" };
    let mut cp_entries: Vec<String> = Vec::new();
    for c in &proc.classpath {
        let p = libraries.join(parse_coord(c)?.rel_path);
        if !p.is_file() {
            bail!("处理器 classpath 依赖缺失：{}（{}）", c, p.display());
        }
        cp_entries.push(p.display().to_string());
    }
    cp_entries.push(proc_jar.display().to_string());
    let classpath = cp_entries.join(sep);

    let mut args = Vec::new();
    for a in &proc.args {
        args.push(parse_literal(a, vars, libraries)?);
    }

    let _ = tx.send(InstallMessage::Status(format!(
        "正在执行安装处理器 {idx}/{total}：{}",
        short_coord(&proc.jar)
    )));

    let output = Command::new(&java.path)
        .arg("-cp")
        .arg(&classpath)
        .arg(&main_class)
        .args(&args)
        .current_dir(&paths.base_dir)
        .output()
        .await
        .context("无法运行安装处理器（检查 Java）")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        bail!(
            "安装处理器 {} 异常退出（退出码 {:?}）：\n{}\n{}",
            short_coord(&proc.jar),
            output.status.code(),
            stderr.trim(),
            stdout.trim()
        );
    }

    // 校验产出物
    for (path, expected) in &resolved_outputs {
        let actual = sha1_file(path)
            .with_context(|| format!("处理器产出文件缺失：{}", path.display()))?;
        if &actual != expected {
            bail!(
                "处理器产出文件校验失败：{}（期望 {expected}，实际 {actual}）",
                path.display()
            );
        }
    }
    Ok(())
}

/* ------------------------------ 工具 ------------------------------ */

fn sha1_file(path: impl AsRef<Path>) -> Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha1::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn absolute(p: &Path) -> Result<PathBuf> {
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()?.join(p)
    };
    Ok(abs)
}

fn short_coord(coord: &str) -> String {
    // 取 name:version（去掉 group 与 classifier），便于日志展示
    let base = coord.split('@').next().unwrap_or(coord);
    let parts: Vec<&str> = base.split(':').collect();
    if parts.len() >= 3 {
        format!("{}:{}", parts[1], parts[2])
    } else {
        coord.to_string()
    }
}
