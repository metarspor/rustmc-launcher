//! 内容管理：模组、资源包、光影、世界与整合包。
//!
//! - 在线：通过 Modrinth API 搜索、下载各类内容。
//! - 本地：导入本地文件（mods / resourcepacks / shaderpacks / saves）。
//! - 整合包：支持 Modrinth `.mrpack` 与 CurseForge 压缩包。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use tokio::sync::mpsc;
use zip::ZipArchive;

use crate::core::download::{self, DownloadEvent, DownloadTask};
use crate::core::modloaders::InstallMessage;
use crate::core::paths::LauncherPaths;

/* ----------------------------- 内容类型 ----------------------------- */

/// 内容种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentKind {
    Mod,
    ResourcePack,
    ShaderPack,
    World,
    Modpack,
}

impl ContentKind {
    pub fn label(self) -> &'static str {
        match self {
            ContentKind::Mod => "模组",
            ContentKind::ResourcePack => "资源包",
            ContentKind::ShaderPack => "光影",
            ContentKind::World => "世界",
            ContentKind::Modpack => "整合包",
        }
    }

    /// 对应 Modrinth project_type 字符串。
    pub fn modrinth_type(self) -> &'static str {
        match self {
            ContentKind::Mod => "mod",
            ContentKind::ResourcePack => "resourcepack",
            ContentKind::ShaderPack => "shader",
            ContentKind::World => "datapack",
            ContentKind::Modpack => "modpack",
        }
    }

    pub fn all() -> [ContentKind; 5] {
        [
            ContentKind::Mod,
            ContentKind::ResourcePack,
            ContentKind::ShaderPack,
            ContentKind::World,
            ContentKind::Modpack,
        ]
    }
}

/* ----------------------------- Modrinth 数据结构 ----------------------------- */

#[derive(Debug, Clone, serde::Deserialize)]
pub struct SearchResponse {
    pub hits: Vec<SearchHit>,
    pub offset: u32,
    pub limit: u32,
    #[serde(rename = "total_hits")]
    pub total_hits: u32,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct SearchHit {
    pub project_id: String,
    pub slug: Option<String>,
    pub title: String,
    pub description: String,
    pub author: String,
    pub downloads: u64,
    pub icon_url: Option<String>,
    #[serde(rename = "project_type")]
    pub project_type: String,
    #[serde(default)]
    pub display_categories: Vec<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ProjectVersion {
    pub id: String,
    pub name: String,
    #[serde(rename = "version_number")]
    pub version_number: String,
    #[serde(default)]
    pub game_versions: Vec<String>,
    #[serde(default)]
    pub loaders: Vec<String>,
    pub files: Vec<VersionFile>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct VersionFile {
    pub url: String,
    pub filename: String,
    pub size: u64,
    pub primary: bool,
    pub hashes: Option<FileHashes>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct FileHashes {
    pub sha1: Option<String>,
    pub sha512: Option<String>,
}

/* ----------------------------- Modrinth 搜索与版本 ----------------------------- */

/// 搜索 Modrinth 内容。
pub async fn search(
    client: &reqwest::Client,
    query: &str,
    kind: ContentKind,
    mc_version: Option<&str>,
    loader: Option<&str>,
    offset: u32,
    limit: u32,
) -> Result<SearchResponse> {
    // 构造 facets：project_type 必选；版本与 loader 可选。
    let mut facets: Vec<Vec<String>> = vec![vec![format!("project_type:{}", kind.modrinth_type())]];
    if let Some(mc) = mc_version {
        facets.push(vec![format!("versions:{mc}")]);
    }
    // 光影/资源包/世界不按 loader 过滤
    if let Some(loader) = loader
        && matches!(kind, ContentKind::Mod | ContentKind::Modpack)
    {
        facets.push(vec![format!("categories:{loader}")]);
    }
    let facets_str = serde_json::to_string(&facets)?;

    let label = "Modrinth 搜索";
    let resp: SearchResponse = crate::core::modloaders::fetch_json_retry(
        || {
            client
                .get("https://api.modrinth.com/v2/search")
                .query(&[
                    ("query", query.to_string()),
                    ("facets", facets_str.clone()),
                    ("offset", offset.to_string()),
                    ("limit", limit.to_string()),
                    ("index", "relevance".to_string()),
                ])
                .header("User-Agent", "RustMC-Launcher/0.1")
        },
        label,
    )
    .await?;
    Ok(resp)
}

/// 获取某项目的可用版本（可按 MC 版本与 loader 过滤）。
pub async fn project_versions(
    client: &reqwest::Client,
    project_id: &str,
    mc_version: Option<&str>,
    loader: Option<&str>,
) -> Result<Vec<ProjectVersion>> {
    let mut gv: Vec<String> = Vec::new();
    let mut ld: Vec<String> = Vec::new();
    if let Some(mc) = mc_version {
        gv.push(mc.to_string());
    }
    if let Some(loader) = loader {
        ld.push(loader.to_string());
    }
    let gv_str = serde_json::to_string(&gv)?;
    let ld_str = serde_json::to_string(&ld)?;
    let label = "Modrinth 项目版本";
    let versions: Vec<ProjectVersion> = crate::core::modloaders::fetch_json_retry(
        || {
            let mut req = client
                .get(format!(
                    "https://api.modrinth.com/v2/project/{project_id}/version"
                ))
                .header("User-Agent", "RustMC-Launcher/0.1");
            if !gv.is_empty() {
                req = req.query(&[("game_versions", gv_str.clone())]);
            }
            if !ld.is_empty() {
                req = req.query(&[("loaders", ld_str.clone())]);
            }
            req
        },
        label,
    )
    .await?;
    Ok(versions)
}

/* ----------------------------- Modrinth 下载 ----------------------------- */

/// 下载并安装一个 Modrinth 版本。
///
/// 世界（datapack）与整合包单独处理；其余按内容类型放入对应目录。
pub async fn install_version(
    client: &reqwest::Client,
    paths: &LauncherPaths,
    game_dir: &Path,
    kind: ContentKind,
    version: &ProjectVersion,
    isolated: bool,
    threads: usize,
    tx: &mpsc::UnboundedSender<InstallMessage>,
) -> Result<()> {
    match kind {
        ContentKind::Modpack => {
            // 下载主 .mrpack 后安装
            let file = pick_primary_file(version)?;
            let target = game_dir.join(".installers").join(&file.filename);
            download_file_to(client, file, &target, threads, tx).await?;
            install_modrinth_pack(client, paths, &target, isolated, threads, tx).await
        }
        ContentKind::World => {
            // 世界文件多为 zip，下载后解压到 saves
            let file = pick_primary_file(version)?;
            let target = game_dir.join(".installers").join(&file.filename);
            download_file_to(client, file, &target, threads, tx).await?;
            import_world_zip(game_dir, &target)
        }
        _ => {
            let dir = content_dir(game_dir, kind);
            std::fs::create_dir_all(&dir)?;
            let mut tasks = Vec::new();
            for file in &version.files {
                let sha1 = file.hashes.as_ref().and_then(|h| h.sha1.clone());
                tasks.push(DownloadTask {
                    url: file.url.clone(),
                    target: dir.join(&file.filename),
                    sha1,
                    size: Some(file.size),
                });
            }
            run_downloads(client.clone(), tasks, threads, tx).await?;
            let _ = tx.send(InstallMessage::Status(format!(
                "{} 已安装：{}",
                kind.label(),
                version.name
            )));
            Ok(())
        }
    }
}

fn pick_primary_file(version: &ProjectVersion) -> Result<&VersionFile> {
    version
        .files
        .iter()
        .find(|f| f.primary)
        .or_else(|| version.files.first())
        .ok_or_else(|| anyhow::anyhow!("该版本没有可下载文件"))
}

/* ----------------------------- 本地文件导入 ----------------------------- */

/// 导入本地文件。
///
/// - 模组：`.jar` 复制到 `mods/`
/// - 资源包 / 光影：`.zip` 复制到对应目录
/// - 世界：`.zip` 解压到 `saves/`
pub fn import_local(
    _paths: &LauncherPaths,
    game_dir: &Path,
    file: &Path,
    kind: ContentKind,
) -> Result<String> {
    let filename = file
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("无效的文件名"))?;

    match kind {
        ContentKind::World => {
            import_world_zip(game_dir, file)?;
            Ok(format!("世界已导入：{filename}"))
        }
        ContentKind::Mod => {
            if !filename.ends_with(".jar") {
                bail!("模组文件必须是 .jar");
            }
            let dir = content_dir(game_dir, ContentKind::Mod);
            std::fs::create_dir_all(&dir)?;
            std::fs::copy(file, dir.join(filename))?;
            Ok(format!("模组已导入：{filename}"))
        }
        ContentKind::ResourcePack | ContentKind::ShaderPack => {
            if !filename.ends_with(".zip") {
                bail!("资源包/光影文件必须是 .zip");
            }
            let dir = content_dir(game_dir, kind);
            std::fs::create_dir_all(&dir)?;
            std::fs::copy(file, dir.join(filename))?;
            Ok(format!("{} 已导入：{filename}", kind.label()))
        }
        ContentKind::Modpack => {
            // 同步入口无法 await，整合包请走 install_pack_from_path（异步）
            bail!("整合包请使用异步安装入口")
        }
    }
}

/// 整合包安装入口（自动识别 Modrinth / CurseForge）。
///
/// `isolated` 为版本隔离开关：开启时整合包模组/配置落到加载器生成的 `versions/<id>/`
/// 目录，关闭时落到游戏根目录（与启动时 game_dir 保持一致）。
pub async fn install_pack_from_path(
    client: &reqwest::Client,
    paths: &LauncherPaths,
    _game_dir: &Path,
    pack: &Path,
    isolated: bool,
    threads: usize,
    tx: &mpsc::UnboundedSender<InstallMessage>,
) -> Result<()> {
    let kind = detect_pack_kind(pack)?;
    match kind {
        ContentKind::Modpack => {
            install_modrinth_pack(client, paths, pack, isolated, threads, tx).await
        }
        _ => install_curseforge_pack(client, paths, pack, isolated, threads, tx).await,
    }
}

/// 解压世界 zip 到 `saves/`（相对于给定游戏数据目录）。
///
/// 兼容两种结构：zip 根目录直接含 `level.dat`，或 zip 内有一个世界文件夹。
fn import_world_zip(game_dir: &Path, zip_path: &Path) -> Result<()> {
    let file = std::fs::File::open(zip_path)
        .with_context(|| format!("打开世界文件失败：{}", zip_path.display()))?;
    let mut archive = ZipArchive::new(file)?;

    // 找到世界根（含 level.dat 的目录层级）
    let mut world_root_prefix: Option<String> = None;
    for i in 0..archive.len() {
        let name = archive.by_index(i)?.name()?.to_string();
        if name.ends_with("level.dat") {
            let prefix = name
                .strip_suffix("level.dat")
                .unwrap_or("")
                .to_string();
            world_root_prefix = Some(prefix);
            break;
        }
    }
    let prefix = world_root_prefix.ok_or_else(|| anyhow::anyhow!("压缩包内未找到 level.dat，不是有效的世界存档"))?;

    // 世界名：取前缀的第一级目录
    let world_name = {
        let trimmed = prefix.trim_end_matches('/');
        if let Some((first, _)) = trimmed.split_once('/') {
            first
        } else {
            // level.dat 在根目录，用 zip 文件名作为世界名
            zip_path
                .file_stem()
                .and_then(|n| n.to_str())
                .unwrap_or("imported_world")
        }
    };
    let saves = game_dir.join("saves");
    let dest = saves.join(world_name);
    std::fs::create_dir_all(&dest)?;

    // 解压
    let file = std::fs::File::open(zip_path)?;
    let mut archive = ZipArchive::new(file)?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name()?.to_string();
        if name.ends_with('/') {
            continue;
        }
        // 计算相对世界根的路径
        let Some(rel) = name.strip_prefix(&prefix) else {
            // 若文件在世界根之外，跳过
            continue;
        };
        if rel.is_empty() {
            continue;
        }
        let out = dest.join(rel);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out_file = std::fs::File::create(&out)?;
        std::io::copy(&mut entry, &mut out_file)?;
    }
    Ok(())
}

/* ----------------------------- 整合包：Modrinth .mrpack ----------------------------- */

#[derive(Debug, serde::Deserialize)]
struct ModrinthPackIndex {
    name: String,
    #[serde(default)]
    files: Vec<PackFile>,
    dependencies: HashMap<String, String>,
}

#[derive(Debug, serde::Deserialize)]
struct PackFile {
    path: String,
    downloads: Vec<String>,
    #[serde(default)]
    file_size: u64,
    #[serde(default)]
    env: HashMap<String, String>,
}

/// 安装 Modrinth `.mrpack`。
///
/// 流程：先解析依赖并自动安装加载器，拿到生成的版本 id；再把模组与 overrides
/// 落到该版本的数据目录（隔离开启时为 `versions/<id>/`，关闭时为游戏根目录）。
async fn install_modrinth_pack(
    client: &reqwest::Client,
    paths: &LauncherPaths,
    pack: &Path,
    isolated: bool,
    threads: usize,
    tx: &mpsc::UnboundedSender<InstallMessage>,
) -> Result<()> {
    let index_text = read_zip_entry(pack, "modrinth.index.json")?;
    let index: ModrinthPackIndex =
        serde_json::from_str(&index_text).context("解析 modrinth.index.json 失败")?;

    let _ = tx.send(InstallMessage::Status(format!(
        "正在安装整合包：{}（{} 个文件）",
        index.name,
        index.files.len()
    )));

    // 1. 先安装加载器，拿到生成的版本 id
    let version_id = install_pack_dependencies(client, paths, &index.dependencies, threads, tx).await?;

    // 2. 计算实际数据目录：隔离开启时落到 versions/<id>/，否则落到根目录
    let data_dir = match &version_id {
        Some(id) if isolated => paths.version_dir(id),
        _ => paths.base_dir.clone(),
    };
    let _ = std::fs::create_dir_all(&data_dir);

    // 3. 收集客户端文件下载任务，落到 data_dir（mrpack 内 path 形如 mods/xxx.jar、config/xxx）
    let mut tasks = Vec::new();
    for f in &index.files {
        let client_needed = f
            .env
            .get("client")
            .map(|v| v != "unsupported")
            .unwrap_or(true);
        if !client_needed {
            continue;
        }
        let Some(url) = f.downloads.first() else {
            continue;
        };
        tasks.push(DownloadTask {
            url: url.clone(),
            target: data_dir.join(&f.path),
            sha1: None,
            size: if f.file_size > 0 { Some(f.file_size) } else { None },
        });
    }
    run_downloads(client.clone(), tasks, threads, tx).await?;

    // 4. 复制 overrides 与 client-overrides 到 data_dir
    copy_override_dir(pack, "overrides", &data_dir)?;
    copy_override_dir(pack, "client-overrides", &data_dir)?;

    let _ = tx.send(InstallMessage::Status(format!(
        "整合包安装完成：{}",
        index.name
    )));
    Ok(())
}

/// 根据整合包依赖自动安装所需的加载器（Forge / NeoForge / Fabric / Quilt）。
///
/// 返回新安装的版本 id（无加载器依赖或安装失败则为 None）。
/// 加载器全部由启动器自动下载安装，无需用户手动到“加载器”页操作；仅在自动
/// 安装失败（网络 / 官方接口异常）时才提示用户手动安装。
async fn install_pack_dependencies(
    client: &reqwest::Client,
    paths: &LauncherPaths,
    deps: &HashMap<String, String>,
    threads: usize,
    tx: &mpsc::UnboundedSender<InstallMessage>,
) -> Result<Option<String>> {
    let Some(mc) = deps.get("minecraft") else {
        return Ok(None);
    };
    if let Some(loader) = deps.get("fabric-loader") {
        let _ = tx.send(InstallMessage::Status(format!(
            "正在自动安装 Fabric {loader}（MC {mc}）…"
        )));
        match crate::core::modloaders::install_fabric(client, paths, mc, loader, tx).await {
            Ok(id) => Ok(Some(id)),
            Err(e) => {
                let _ = tx.send(InstallMessage::Status(format!(
                    "Fabric 自动安装失败：{e:#}，请在“加载器”页手动安装"
                )));
                Ok(None)
            }
        }
    } else if let Some(loader) = deps.get("quilt-loader") {
        let _ = tx.send(InstallMessage::Status(format!(
            "正在自动安装 Quilt {loader}（MC {mc}）…"
        )));
        match crate::core::modloaders::install_quilt(client, paths, mc, loader, tx).await {
            Ok(id) => Ok(Some(id)),
            Err(e) => {
                let _ = tx.send(InstallMessage::Status(format!(
                    "Quilt 自动安装失败：{e:#}，请在“加载器”页手动安装"
                )));
                Ok(None)
            }
        }
    } else if let Some(v) = deps.get("neoforge") {
        let _ = tx.send(InstallMessage::Status(format!(
            "正在自动安装 NeoForge {v}（MC {mc}）…"
        )));
        match crate::core::modloaders::install_neoforge(client, paths, v, threads, None, tx).await {
            Ok(id) => Ok(Some(id)),
            Err(e) => {
                let _ = tx.send(InstallMessage::Status(format!(
                    "NeoForge 自动安装失败：{e:#}，请在“加载器”页手动安装"
                )));
                Ok(None)
            }
        }
    } else if let Some(v) = deps.get("forge") {
        // Modrinth 的 forge 依赖只有构建版本（如 47.2.0），需补 MC 前缀
        let full = if v.contains('-') {
            v.clone()
        } else {
            format!("{mc}-{v}")
        };
        let _ = tx.send(InstallMessage::Status(format!(
            "正在自动安装 Forge {full}…"
        )));
        match crate::core::modloaders::install_forge(client, paths, &full, threads, None, tx).await {
            Ok(id) => Ok(Some(id)),
            Err(e) => {
                let _ = tx.send(InstallMessage::Status(format!(
                    "Forge 自动安装失败：{e:#}，请在“加载器”页手动安装"
                )));
                Ok(None)
            }
        }
    } else {
        Ok(None)
    }
}

/* ----------------------------- 整合包：CurseForge ----------------------------- */

#[derive(Debug, serde::Deserialize)]
struct CurseManifest {
    name: String,
    minecraft: CurseMinecraft,
    #[serde(default)]
    files: Vec<CurseFile>,
    #[serde(default)]
    overrides: String,
}

#[derive(Debug, serde::Deserialize)]
#[allow(non_snake_case)]
struct CurseMinecraft {
    version: String,
    #[serde(default)]
    modLoaders: Vec<CurseModLoader>,
}

#[derive(Debug, serde::Deserialize)]
struct CurseModLoader {
    id: String,
}

#[derive(Debug, serde::Deserialize)]
#[allow(non_snake_case)]
struct CurseFile {
    projectID: u64,
    fileID: u64,
    #[serde(default)]
    #[allow(dead_code)]
    required: bool,
}

/// 安装 CurseForge 整合包。
///
/// 先自动安装加载器并拿到版本 id，再把模组与 overrides 落到该版本的数据目录
/// （隔离开启时为 `versions/<id>/`，关闭时为游戏根目录）。
async fn install_curseforge_pack(
    client: &reqwest::Client,
    paths: &LauncherPaths,
    pack: &Path,
    isolated: bool,
    threads: usize,
    tx: &mpsc::UnboundedSender<InstallMessage>,
) -> Result<()> {
    let manifest_text = read_zip_entry(pack, "manifest.json")?;
    let manifest: CurseManifest =
        serde_json::from_str(&manifest_text).context("解析 manifest.json 失败")?;

    let _ = tx.send(InstallMessage::Status(format!(
        "正在安装 CurseForge 整合包：{}（{} 个文件）",
        manifest.name,
        manifest.files.len()
    )));

    // 1. 先安装加载器，拿到生成的版本 id
    let mc = manifest.minecraft.version.clone();
    let mut version_id: Option<String> = None;
    for ml in &manifest.minecraft.modLoaders {
        let id = ml.id.trim();
        // id 形如 "forge-47.3.0"、"neoforge-21.1.0"、"fabric-0.16.5"、"quilt-0.26.0"
        let Some((kind, ver)) = id.split_once('-') else {
            let _ = tx.send(InstallMessage::Status(format!(
                "无法解析加载器 {id}，请在“加载器”页手动安装（MC {mc}）"
            )));
            continue;
        };
        let installed = match kind.to_ascii_lowercase().as_str() {
            "fabric" => {
                let _ = tx.send(InstallMessage::Status(format!(
                    "正在自动安装 Fabric {ver}（MC {mc}）…"
                )));
                crate::core::modloaders::install_fabric(client, paths, &mc, ver, tx).await.ok()
            }
            "quilt" => {
                let _ = tx.send(InstallMessage::Status(format!(
                    "正在自动安装 Quilt {ver}（MC {mc}）…"
                )));
                crate::core::modloaders::install_quilt(client, paths, &mc, ver, tx).await.ok()
            }
            "neoforge" => {
                let _ = tx.send(InstallMessage::Status(format!(
                    "正在自动安装 NeoForge {ver}（MC {mc}）…"
                )));
                crate::core::modloaders::install_neoforge(client, paths, ver, threads, None, tx).await.ok()
            }
            "forge" => {
                let full = format!("{mc}-{ver}");
                let _ = tx.send(InstallMessage::Status(format!(
                    "正在自动安装 Forge {full}…"
                )));
                crate::core::modloaders::install_forge(client, paths, &full, threads, None, tx).await.ok()
            }
            other => {
                let _ = tx.send(InstallMessage::Status(format!(
                    "未知加载器 {other}（{id}），请在“加载器”页手动安装（MC {mc}）"
                )));
                None
            }
        };
        if installed.is_some() && version_id.is_none() {
            version_id = installed;
        }
    }

    // 2. 计算实际数据目录
    let data_dir = match &version_id {
        Some(id) if isolated => paths.version_dir(id),
        _ => paths.base_dir.clone(),
    };
    let _ = std::fs::create_dir_all(&data_dir);

    // 3. 构造下载任务（公开端点，可能被 Cloudflare 拦截，失败则记录）
    let tmp_dir = paths.base_dir.join(".installers");
    std::fs::create_dir_all(&tmp_dir)?;
    let mut tasks = Vec::new();
    for f in &manifest.files {
        let url = format!(
            "https://www.curseforge.com/api/v1/mods/{}/files/{}/download",
            f.projectID, f.fileID
        );
        tasks.push(DownloadTask {
            url,
            target: tmp_dir.join(format!("cf-{}-{}.jar", f.projectID, f.fileID)),
            sha1: None,
            size: None,
        });
    }

    // 并发尽力下载：download::run 会把能下载的都下载完，仅在最后返回错误
    let _ = run_downloads(client.clone(), tasks.clone(), threads, tx).await;

    // 检查哪些文件缺失（失败）
    let mut downloaded = Vec::new();
    let mut failed_count = 0u32;
    for task in &tasks {
        if task.target.exists() {
            downloaded.push(task.target.clone());
        } else {
            failed_count += 1;
        }
    }

    // 把成功下载的 jar 移到 data_dir/mods
    let mods = data_dir.join("mods");
    std::fs::create_dir_all(&mods)?;
    for f in downloaded {
        if let Some(name) = f.file_name() {
            let _ = std::fs::copy(&f, mods.join(name));
            let _ = std::fs::remove_file(&f);
        }
    }

    // 复制 overrides 到 data_dir
    let override_dir = if manifest.overrides.is_empty() {
        "overrides"
    } else {
        manifest.overrides.as_str()
    };
    copy_override_dir(pack, override_dir, &data_dir)?;

    if failed_count > 0 {
        let _ = tx.send(InstallMessage::Status(format!(
            "整合包已安装，但 {failed_count} 个文件下载失败（可能被防拦截），请手动补全"
        )));
    } else {
        let _ = tx.send(InstallMessage::Status(format!(
            "CurseForge 整合包安装完成：{}",
            manifest.name
        )));
    }
    Ok(())
}

/* ----------------------------- 已安装内容列表 ----------------------------- */

/// 列出某目录下的内容文件名（不含目录）。
fn list_dir_entries(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if e.path().is_file()
                && let Some(name) = e.file_name().to_str()
            {
                out.push(name.to_string());
            }
        }
    }
    out.sort();
    out
}

pub fn list_installed(_paths: &LauncherPaths, game_dir: &Path, kind: ContentKind) -> Vec<String> {
    match kind {
        ContentKind::World => {
            let saves = game_dir.join("saves");
            let mut out = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&saves) {
                for e in entries.flatten() {
                    if e.path().is_dir()
                        && e.path().join("level.dat").exists()
                        && let Some(name) = e.file_name().to_str()
                    {
                        out.push(name.to_string());
                    }
                }
            }
            out.sort();
            out
        }
        _ => list_dir_entries(&content_dir(game_dir, kind)),
    }
}

/// 删除一个已安装内容。
pub fn delete_installed(_paths: &LauncherPaths, game_dir: &Path, kind: ContentKind, name: &str) -> Result<()> {
    match kind {
        ContentKind::World => {
            let p = game_dir.join("saves").join(name);
            std::fs::remove_dir_all(&p)
                .with_context(|| format!("删除世界失败：{name}"))?;
        }
        _ => {
            let p = content_dir(game_dir, kind).join(name);
            std::fs::remove_file(&p)
                .with_context(|| format!("删除失败：{name}"))?;
        }
    }
    Ok(())
}

/* ----------------------------- 路径与压缩辅助 ----------------------------- */

/// 内容类型对应的目录。
///
/// `game_dir` 是当前活动的游戏数据目录：版本隔离开启时为 `versions/<id>/`，
/// 关闭时为游戏根目录。所有 mods / resourcepacks / shaderpacks / saves 都落到这里。
pub fn content_dir(game_dir: &Path, kind: ContentKind) -> PathBuf {
    match kind {
        ContentKind::Mod => game_dir.join("mods"),
        ContentKind::ResourcePack => game_dir.join("resourcepacks"),
        ContentKind::ShaderPack => game_dir.join("shaderpacks"),
        ContentKind::World => game_dir.join("saves"),
        ContentKind::Modpack => game_dir.join(".installers"),
    }
}

/// 读取 zip 内某文件的文本内容。
fn read_zip_entry(zip_path: &Path, entry_name: &str) -> Result<String> {
    let file = std::fs::File::open(zip_path)
        .with_context(|| format!("打开压缩包失败：{}", zip_path.display()))?;
    let mut archive = ZipArchive::new(file)?;
    let mut entry = archive
        .by_name(entry_name)
        .with_context(|| format!("压缩包内未找到 {entry_name}"))?;
    let mut text = String::new();
    std::io::Read::read_to_string(&mut entry, &mut text)?;
    Ok(text)
}

/// 把 zip 内某个目录（如 overrides）整体复制到目标游戏数据目录。
fn copy_override_dir(zip_path: &Path, dir_name: &str, dest_root: &Path) -> Result<()> {
    let file = std::fs::File::open(zip_path)?;
    let mut archive = ZipArchive::new(file)?;
    let prefix = format!("{dir_name}/");

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name()?.to_string();
        if !name.starts_with(&prefix) || name.ends_with('/') {
            continue;
        }
        let rel = &name[prefix.len()..];
        if rel.is_empty() {
            continue;
        }
        let out = dest_root.join(rel);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out_file = std::fs::File::create(&out)?;
        std::io::copy(&mut entry, &mut out_file)?;
    }
    Ok(())
}

/// 识别整合包类型：含 modrinth.index.json 为 Modrinth，否则视为 CurseForge。
fn detect_pack_kind(zip_path: &Path) -> Result<ContentKind> {
    let file = std::fs::File::open(zip_path)?;
    let mut archive = ZipArchive::new(file)?;
    if archive.by_name("modrinth.index.json").is_ok() {
        Ok(ContentKind::Modpack)
    } else if archive.by_name("manifest.json").is_ok() {
        // 用一个非 Modpack 的标记区分（CurseForge 分支）
        Ok(ContentKind::World)
    } else {
        bail!("无法识别整合包格式（缺少 modrinth.index.json 或 manifest.json）")
    }
}

/// 下载单个文件到指定路径（转发下载事件）。
async fn download_file_to(
    client: &reqwest::Client,
    file: &VersionFile,
    target: &Path,
    threads: usize,
    tx: &mpsc::UnboundedSender<InstallMessage>,
) -> Result<()> {
    let task = DownloadTask {
        url: file.url.clone(),
        target: target.to_path_buf(),
        sha1: file.hashes.as_ref().and_then(|h| h.sha1.clone()),
        size: Some(file.size),
    };
    run_downloads(client.clone(), vec![task], threads, tx).await
}

/// 运行下载任务并转发事件。
async fn run_downloads(
    client: reqwest::Client,
    tasks: Vec<DownloadTask>,
    threads: usize,
    tx: &mpsc::UnboundedSender<InstallMessage>,
) -> Result<()> {
    let (dl_tx, mut dl_rx) = mpsc::unbounded_channel::<DownloadEvent>();
    let fwd = tx.clone();
    let h = tokio::spawn(async move {
        while let Some(e) = dl_rx.recv().await {
            let _ = fwd.send(InstallMessage::Download(e));
        }
    });
    let result = download::run(client, tasks, dl_tx, threads.max(1)).await;
    h.abort();
    let _ = dl_rx;
    result
}
