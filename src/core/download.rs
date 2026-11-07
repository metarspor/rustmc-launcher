//! 下载管理：收集客户端、库、资源、natives 等任务，并发下载并校验。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures::StreamExt;
use sha1::{Digest, Sha1};
use tokio::sync::{mpsc, Semaphore};
use zip::ZipArchive;

use crate::core::manifest::{
    Artifact, AssetIndexData, Library, VersionDetail,
};
use crate::core::paths::LauncherPaths;
use crate::core::rules::{self, EnvInfo};

/// 单个下载任务。
#[derive(Debug, Clone)]
pub struct DownloadTask {
    pub url: String,
    pub target: PathBuf,
    pub sha1: Option<String>,
    pub size: Option<u64>,
}

/// 下载进度事件（通过 channel 发给 TUI）。
#[derive(Debug, Clone)]
pub enum DownloadEvent {
    Start {
        total_files: u64,
        total_bytes: u64,
    },
    FileDone {
        completed: u64,
        total: u64,
        finished_bytes: u64,
        total_bytes: u64,
        name: String,
    },
    #[allow(dead_code)]
    Message(String),
    Finish,
}

/* ---------------------------- 收集任务 ---------------------------- */

/// 根据版本详情收集所有需要的下载任务，同时确保资源索引就绪。
pub async fn collect_tasks(
    client: &reqwest::Client,
    paths: &LauncherPaths,
    detail: &VersionDetail,
    env: &EnvInfo,
) -> Result<(Vec<DownloadTask>, Option<AssetIndexData>)> {
    let mut tasks = Vec::new();

    // 1. 客户端 jar
    if let Some(client_dl) = detail.downloads.as_ref().and_then(|d| d.get("client")) {
        tasks.push(DownloadTask {
            url: client_dl.url.clone(),
            target: paths.version_jar(&detail.id),
            sha1: client_dl.sha1.clone(),
            size: client_dl.size,
        });
    }

    // 2. 资源索引（先确保下载到本地，再解析）
    let asset_index = ensure_asset_index(client, paths, detail).await?;
    if let Some((data, _)) = &asset_index {
        // 3. 资源对象
        for obj in data.objects.values() {
            tasks.push(DownloadTask {
                url: format!(
                    "https://resources.download.minecraft.net/{}/{}",
                    obj.hash.get(..2).unwrap_or("xx"),
                    obj.hash
                ),
                target: paths.asset_object(&obj.hash),
                sha1: Some(obj.hash.clone()),
                size: Some(obj.size),
            });
        }
    }

    // 4. 库
    for lib in detail.libraries.iter().flatten() {
        tasks.extend(library_tasks(lib, paths, env));
    }

    // 5. log4j 配置
    if let Some(logging) = &detail.logging
        && let Some(client) = &logging.client
        && let Some(task) = logging_task(&client.file, paths) {
            tasks.push(task);
        }

    Ok((tasks, asset_index.map(|(d, _)| d)))
}

async fn ensure_asset_index(
    client: &reqwest::Client,
    paths: &LauncherPaths,
    detail: &VersionDetail,
) -> Result<Option<(AssetIndexData, PathBuf)>> {
    let Some(info) = detail.asset_index.as_ref() else {
        return Ok(None);
    };
    let target = paths.asset_index_path(&info.id);
    if !target.exists() {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = client
            .get(&info.url)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        std::fs::write(&target, &bytes)?;
    }
    let text = std::fs::read_to_string(&target)?;
    let data: AssetIndexData = serde_json::from_str(&text)?;
    Ok(Some((data, target)))
}

fn library_tasks(lib: &Library, paths: &LauncherPaths, env: &EnvInfo) -> Vec<DownloadTask> {
    let mut tasks = Vec::new();
    if !rules::allowed(lib.rules.as_ref(), env) {
        return tasks;
    }

    // 旧版（lwjgl 2）natives classifiers
    if let Some(natives_map) = &lib.natives
        && let Some(key) = natives_map.get(env.os_name) {
            let key = key.replace("${arch}", env.arch);
            if let Some(artifact) = lib
                .downloads
                .as_ref()
                .and_then(|d| d.classifiers.as_ref())
                .and_then(|c| c.get(&key))
                && let Some(task) = artifact_to_task(artifact, paths) {
                    tasks.push(task);
                }
        }

    // 主 artifact（同时覆盖新版 natives 独立条目）
    if let Some(artifact) = lib.downloads.as_ref().and_then(|d| d.artifact.as_ref()) {
        if let Some(task) = artifact_to_task(artifact, paths) {
            tasks.push(task);
        }
    } else if let Some(base_url) = &lib.url {
        // Forge 风格：无 downloads，用 maven 坐标拼 URL
        if let Some(rel) = LauncherPaths::library_rel_path(&lib.name) {
            tasks.push(DownloadTask {
                url: format!("{}/{}", base_url.trim_end_matches('/'), rel),
                target: paths.libraries_dir().join(rel),
                sha1: None,
                size: None,
            });
        }
    }

    tasks
}

fn artifact_to_task(artifact: &Artifact, paths: &LauncherPaths) -> Option<DownloadTask> {
    let url = artifact.url.clone()?;
    let target = if let Some(rel) = &artifact.path {
        paths.libraries_dir().join(rel)
    } else {
        return None;
    };
    Some(DownloadTask {
        url,
        target,
        sha1: artifact.sha1.clone(),
        size: artifact.size,
    })
}

fn logging_task(file: &Artifact, paths: &LauncherPaths) -> Option<DownloadTask> {
    let url = file.url.clone()?;
    let name = file.id.clone().unwrap_or_else(|| "client.xml".to_string());
    Some(DownloadTask {
        url,
        target: paths.log_configs_dir().join(name),
        sha1: file.sha1.clone(),
        size: file.size,
    })
}

/* ---------------------------- 执行下载 ---------------------------- */

/// 并发执行下载任务。已存在且大小匹配的文件会被跳过。
pub async fn run(
    client: reqwest::Client,
    tasks: Vec<DownloadTask>,
    tx: mpsc::UnboundedSender<DownloadEvent>,
    concurrency: usize,
) -> Result<()> {
    let pending: Vec<DownloadTask> =
        tasks.into_iter().filter(needs_download).collect();

    let total_files = pending.len() as u64;
    let total_bytes = pending.iter().filter_map(|t| t.size).sum();
    tx.send(DownloadEvent::Start {
        total_files,
        total_bytes,
    })?;

    let semaphore = Arc::new(Semaphore::new(concurrency));
    let completed = Arc::new(AtomicU64::new(0));
    let finished_bytes = Arc::new(AtomicU64::new(0));

    let results = futures::stream::iter(pending)
        .map(|task| {
            let semaphore = semaphore.clone();
            let completed = completed.clone();
            let finished_bytes = finished_bytes.clone();
            let tx = tx.clone();
            let client = client.clone();
            async move {
                let _permit = semaphore
                    .acquire_owned()
                    .await
                    .context("获取下载许可失败")?;
                let name = task
                    .target
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("?")
                    .to_string();
                download_one(&client, &task).await?;

                let c = completed.fetch_add(1, Ordering::SeqCst) + 1;
                let b = finished_bytes
                    .fetch_add(task.size.unwrap_or(0), Ordering::SeqCst)
                    + task.size.unwrap_or(0);
                tx.send(DownloadEvent::FileDone {
                    completed: c,
                    total: total_files,
                    finished_bytes: b,
                    total_bytes,
                    name,
                })?;
                Ok::<(), anyhow::Error>(())
            }
        })
        .buffer_unordered(concurrency);

    let outcomes: Vec<_> = outcomes_collect(results).await;
    // 容错收集：单个文件失败不中断其它文件，最后统一报告失败清单。
    let mut failed: Vec<String> = Vec::new();
    for outcome in outcomes {
        if let Err(e) = outcome {
            failed.push(format!("{e:#}"));
        }
    }

    tx.send(DownloadEvent::Finish)?;

    if !failed.is_empty() {
        bail!(
            "下载完成，但 {} 个文件失败：\n  - {}",
            failed.len(),
            failed.join("\n  - ")
        );
    }
    Ok(())
}

async fn outcomes_collect(
    mut s: impl StreamExt<Item = Result<()>> + Unpin,
) -> Vec<Result<()>> {
    let mut out = Vec::new();
    while let Some(item) = s.next().await {
        out.push(item);
    }
    out
}

fn needs_download(task: &DownloadTask) -> bool {
    match std::fs::metadata(&task.target) {
        Ok(meta) => {
            if let Some(s) = task.size {
                // 已知大小：大小不一致则需要下载
                s != meta.len()
            } else if task.sha1.is_some() {
                // 仅知哈希：校验已有文件的 SHA1
                !verify_existing_sha1(&task.target, task.sha1.as_deref().unwrap())
            } else {
                // 大小与哈希均未知：信任已存在的文件，避免重复下载
                false
            }
        }
        Err(_) => true,
    }
}

/// 计算已存在文件的 SHA1 并与期望值比较。
fn verify_existing_sha1(path: &Path, expected: &str) -> bool {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut hasher = Sha1::new();
    let mut buf = [0u8; 65536];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(_) => return false,
        }
    }
    hex::encode(hasher.finalize()) == expected
}

/// 下载单个文件，最多重试 3 次。
async fn download_one(client: &reqwest::Client, task: &DownloadTask) -> Result<()> {
    if let Some(parent) = task.target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file_name = task
        .target
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("download");
    let tmp = task
        .target
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(".{file_name}.part"));

    let mut last_err = None;
    for attempt in 1..=3 {
        match try_download(client, task, &tmp).await {
            Ok(()) => {
                std::fs::rename(&tmp, &task.target).with_context(|| {
                    format!("写入文件失败：{}", task.target.display())
                })?;
                return Ok(());
            }
            Err(e) => {
                last_err = Some(e);
                if attempt < 3 {
                    tokio::time::sleep(Duration::from_millis(500 * attempt)).await;
                }
            }
        }
    }
    let _ = std::fs::remove_file(&tmp);
    bail!(
        "下载失败（已重试 3 次）：{} —— {:?}",
        task.url,
        last_err
    )
}

async fn try_download(
    client: &reqwest::Client,
    task: &DownloadTask,
    tmp: &Path,
) -> Result<()> {
    let resp = client
        .get(&task.url)
        .send()
        .await?
        .error_for_status()
        .with_context(|| format!("请求失败：{}", task.url))?;
    let bytes = resp.bytes().await?;

    if let Some(expected) = &task.sha1 {
        let mut hasher = Sha1::new();
        hasher.update(&bytes);
        let actual = hex::encode(hasher.finalize());
        if expected != &actual {
            bail!("SHA1 校验失败：{}", task.target.display());
        }
    }

    std::fs::write(tmp, &bytes)?;
    Ok(())
}

/* ------------------------- natives 解压 ------------------------- */

/// 解压当前环境所需的全部 native jar。
///
/// 现代版本（LWJGL 3.3+）主要靠 classpath 上的 native jar 自动提取，
/// 这里同时把库扁平化解压到 `java.library.path` 指向的目录作为兜底；
/// 旧版本（LWJGL 2）则完全依赖该目录。
pub fn extract_natives(
    paths: &LauncherPaths,
    detail: &VersionDetail,
    env: &EnvInfo,
) -> Result<()> {
    let natives_dir = paths.natives_dir(&detail.id);
    // 清空旧的解压结果，保证重复执行幂等且不残留 .git/.sha1 等辅助文件
    let _ = std::fs::remove_dir_all(&natives_dir);
    std::fs::create_dir_all(&natives_dir)?;

    // java.library.path 实际指向的目录（新版可能是 natives/java）
    let java_dir = resolve_java_library_dir(paths, detail, env);
    std::fs::create_dir_all(&java_dir)?;

    for lib in detail.libraries.iter().flatten() {
        if !rules::allowed(lib.rules.as_ref(), env) {
            continue;
        }
        let excludes = lib
            .extract
            .as_ref()
            .and_then(|e| e.exclude.clone())
            .unwrap_or_default();

        // 旧版 classifiers
        if let Some(natives_map) = &lib.natives
            && let Some(key) = natives_map.get(env.os_name) {
                let key = key.replace("${arch}", env.arch);
                if let Some(artifact) = lib
                    .downloads
                    .as_ref()
                    .and_then(|d| d.classifiers.as_ref())
                    .and_then(|c| c.get(&key))
                    .and_then(|a| a.path.as_ref())
                {
                    let jar = paths.libraries_dir().join(artifact);
                    extract_jar(&jar, &java_dir, &excludes)?;
                }
            }

        // 新版独立 natives 条目
        if is_natives_coordinate(&lib.name)
            && let Some(rel) = lib
                .downloads
                .as_ref()
                .and_then(|d| d.artifact.as_ref())
                .and_then(|a| a.path.as_ref())
            {
                let jar = paths.libraries_dir().join(rel);
                extract_jar(&jar, &java_dir, &excludes)?;
            }
    }
    Ok(())
}

/// 解析版本 JVM 参数中 `java.library.path` 指向的实际目录。
///
/// 模板可能是 `${natives_directory}`（旧版）或 `${natives_directory}/java`（新版）。
fn resolve_java_library_dir(
    paths: &LauncherPaths,
    detail: &VersionDetail,
    env: &EnvInfo,
) -> PathBuf {
    let natives_dir = paths.natives_dir(&detail.id);
    let mut target = natives_dir.clone();

    if let Some(arguments) = &detail.arguments
        && let Some(jvm_args) = &arguments.jvm
    {
        for arg in jvm_args {
            for raw in arg.expand(env) {
                if let Some(rest) = raw.strip_prefix("-Djava.library.path=")
                    && let Some(sub) = rest.strip_prefix("${natives_directory}/")
                {
                    target = natives_dir.join(sub);
                }
            }
        }
    }
    target
}

/// maven 坐标的 classifier 以 `natives-` 开头（新版 lwjgl 3）。
fn is_natives_coordinate(name: &str) -> bool {
    name.split(':')
        .nth(3)
        .map(|c| c.starts_with("natives-"))
        .unwrap_or(false)
}

fn extract_jar(jar: &Path, dest: &Path, excludes: &[String]) -> Result<()> {
    let file = std::fs::File::open(jar)
        .with_context(|| format!("打开 native jar 失败：{}", jar.display()))?;
    let mut archive = ZipArchive::new(file)?;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name()?.to_string();

        if name.ends_with('/') || excludes.iter().any(|e| name.starts_with(e)) {
            continue;
        }
        // 只提取原生动态库，排除 jar 内的 .git/.sha1 等辅助文件
        if !is_native_file(&name) {
            continue;
        }

        // 扁平化：只取文件名。LWJGL 3 native jar 内的库位于
        // `linux/x64/org/lwjgl/...` 深层路径，需要拍平到目标目录。
        let Some(file_name) = Path::new(&name).file_name() else {
            continue;
        };
        let out = dest.join(file_name);
        // 防止 zip slip 路径穿越（file_name 不含分隔符，此处为双保险）
        if !out.starts_with(dest) {
            continue;
        }
        let mut out_file = std::fs::File::create(&out)?;
        std::io::copy(&mut entry, &mut out_file)?;
    }
    Ok(())
}

/// 判断是否为原生动态库文件。
///
/// 接受：`.so`、`.dll`、`.dylib`，以及带数字版本号的 `.so.6` / `.so.6.3.0`；
/// 排除 lwjgl native jar 中的 `.git`、`.sha1` 等辅助文件。
fn is_native_file(name: &str) -> bool {
    if name.ends_with(".so")
        || name.ends_with(".dll")
        || name.ends_with(".dylib")
    {
        return true;
    }
    if let Some(pos) = name.find(".so.") {
        let after = &name[pos + 4..];
        // 版本号必须以数字开头
        return after
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false);
    }
    false
}
