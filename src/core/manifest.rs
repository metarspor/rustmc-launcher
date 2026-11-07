//! Mojang 版本清单、版本详情、资源索引的数据结构与加载。
//!
//! 结构体完整保留了 Mojang JSON 的字段（即使当前未全部读取），
//! 因此对整个模块允许 dead_code。
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::core::paths::LauncherPaths;

const VERSION_MANIFEST_URL: &str =
    "https://launchermeta.mojang.com/mc/game/version_manifest_v2.json";

/* ----------------------------- 版本清单 ----------------------------- */

#[derive(Debug, Clone, Deserialize)]
pub struct VersionManifest {
    pub latest: Latest,
    pub versions: Vec<VersionEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Latest {
    pub release: String,
    pub snapshot: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VersionEntry {
    pub id: String,
    #[serde(rename = "type")]
    pub version_type: String,
    pub url: String,
    pub sha1: String,
    #[serde(rename = "releaseTime")]
    pub release_time: String,
}

/* ----------------------------- 版本详情 ----------------------------- */

#[derive(Debug, Clone, Deserialize)]
pub struct VersionDetail {
    pub id: String,
    #[serde(rename = "mainClass")]
    pub main_class: Option<String>,
    #[serde(rename = "minimumLauncherVersion")]
    pub minimum_launcher_version: Option<u32>,
    #[serde(rename = "type")]
    pub version_type: Option<String>,
    pub assets: Option<String>,
    #[serde(rename = "assetIndex")]
    pub asset_index: Option<AssetIndexInfo>,
    pub downloads: Option<HashMap<String, Download>>,
    pub libraries: Option<Vec<Library>>,
    pub arguments: Option<Arguments>,
    #[serde(rename = "minecraftArguments")]
    pub minecraft_arguments: Option<String>,
    #[serde(rename = "javaVersion")]
    pub java_version: Option<JavaVersionInfo>,
    pub logging: Option<LoggingConfig>,
    #[serde(rename = "inheritsFrom")]
    pub inherits_from: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssetIndexInfo {
    pub id: String,
    pub sha1: String,
    pub url: String,
    #[serde(rename = "totalSize")]
    pub total_size: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Download {
    pub sha1: Option<String>,
    pub size: Option<u64>,
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JavaVersionInfo {
    pub component: Option<String>,
    #[serde(rename = "majorVersion")]
    pub major_version: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Library {
    pub name: String,
    pub downloads: Option<LibraryDownloads>,
    pub rules: Option<Vec<Rule>>,
    /// 旧版 natives 映射：`linux` -> `natives-linux`
    pub natives: Option<HashMap<String, String>>,
    pub extract: Option<ExtractRule>,
    /// Forge 等自定义 Maven 仓库基址
    pub url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LibraryDownloads {
    pub artifact: Option<Artifact>,
    pub classifiers: Option<HashMap<String, Artifact>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Artifact {
    pub id: Option<String>,
    pub path: Option<String>,
    pub sha1: Option<String>,
    pub size: Option<u64>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExtractRule {
    pub exclude: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Arguments {
    pub game: Vec<ArgumentValue>,
    pub jvm: Option<Vec<ArgumentValue>>,
}

/// 启动参数：要么是纯字符串，要么是带规则的条件参数。
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ArgumentValue {
    Plain(String),
    Conditional {
        rules: Vec<Rule>,
        value: OneOrMany,
    },
}

impl ArgumentValue {
    /// 展开为一个或多个字符串（条件不满足时返回空）。
    pub fn expand(&self, env: &crate::core::rules::EnvInfo) -> Vec<String> {
        match self {
            ArgumentValue::Plain(s) => vec![s.clone()],
            ArgumentValue::Conditional { rules, value } => {
                if crate::core::rules::evaluate(rules, env) {
                    match value {
                        OneOrMany::One(s) => vec![s.clone()],
                        OneOrMany::Many(v) => v.clone(),
                    }
                } else {
                    Vec::new()
                }
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

#[derive(Debug, Clone, Deserialize)]
pub struct Rule {
    pub action: String,
    pub os: Option<OsRule>,
    pub features: Option<HashMap<String, bool>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OsRule {
    pub name: Option<String>,
    pub version: Option<String>,
    pub arch: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoggingConfig {
    /// 加载器版本（Forge/Fabric 等）可能给出空的 `logging: {}`，故为可选。
    pub client: Option<LoggingClient>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoggingClient {
    pub argument: String,
    pub file: Artifact,
}

/* ----------------------------- 资源索引 ----------------------------- */

#[derive(Debug, Clone, Deserialize)]
pub struct AssetIndexData {
    pub objects: HashMap<String, AssetObject>,
    /// 极老版本（1.6 之前）资源直接放到 resources 目录
    pub r#virtual: Option<bool>,
    #[serde(rename = "map_to_resources")]
    pub map_to_resources: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssetObject {
    pub hash: String,
    pub size: u64,
}

/* ----------------------------- 网络加载 ----------------------------- */

/// 拉取并解析官方版本清单。
pub async fn fetch_manifest(client: &reqwest::Client) -> Result<VersionManifest> {
    let manifest: VersionManifest = client
        .get(VERSION_MANIFEST_URL)
        .send()
        .await
        .context("请求版本清单失败（检查网络连接）")?
        .error_for_status()
        .context("版本清单服务器返回错误")?
        .json()
        .await
        .context("解析版本清单失败")?;
    Ok(manifest)
}

/// 拉取版本详情 JSON。
pub async fn fetch_version_detail(
    client: &reqwest::Client,
    url: &str,
) -> Result<VersionDetail> {
    let detail: VersionDetail = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("请求版本详情失败：{url}"))?
        .error_for_status()?
        .json()
        .await
        .context("解析版本详情失败")?;
    Ok(detail)
}

/// 拉取资源索引。
pub async fn fetch_asset_index(
    client: &reqwest::Client,
    url: &str,
) -> Result<AssetIndexData> {
    let data: AssetIndexData = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("请求资源索引失败：{url}"))?
        .error_for_status()?
        .json()
        .await
        .context("解析资源索引失败")?;
    Ok(data)
}

/* --------------------------- 本地读写与继承 --------------------------- */

/// 读取本地版本 JSON。
pub fn read_local_version(path: &Path) -> Result<VersionDetail> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("读取本地版本失败：{}", path.display()))?;
    let detail: VersionDetail =
        serde_json::from_str(&text).context("解析本地版本 JSON 失败")?;
    Ok(detail)
}

/// 加载版本并递归合并 `inheritsFrom`（Forge / Fabric / NeoForge 等）。
///
/// `manifest` 仅在本地不存在该版本、需要从在线清单下载版本 JSON 时使用；
/// 已安装的本地版本不需要在线清单。
pub async fn load_resolved_version(
    id: &str,
    paths: &LauncherPaths,
    client: &reqwest::Client,
    manifest: Option<&VersionManifest>,
) -> Result<VersionDetail> {
    let local = paths.version_json(id);
    let detail = if local.exists() {
        read_local_version(&local)?
    } else {
        let entry = manifest
            .and_then(|m| m.versions.iter().find(|v| v.id == id))
            .with_context(|| format!("本地未安装版本 {id}，且在线版本清单不可用"))?;
        // 保存原始 JSON 文本，避免为整个结构实现 Serialize
        let text = client
            .get(&entry.url)
            .send()
            .await
            .with_context(|| format!("下载版本 JSON 失败：{}", entry.url))?
            .error_for_status()?
            .text()
            .await?;
        std::fs::create_dir_all(paths.version_dir(id))?;
        std::fs::write(&local, &text)?;
        serde_json::from_str(&text)?
    };

    match &detail.inherits_from {
        Some(parent_id) => {
            let parent =
                Box::pin(load_resolved_version(parent_id, paths, client, manifest)).await?;
            Ok(merge_version(parent, detail))
        }
        None => Ok(detail),
    }
}

/// 把父版本与子版本合并，子版本优先。
fn merge_version(parent: VersionDetail, child: VersionDetail) -> VersionDetail {
    VersionDetail {
        id: child.id,
        main_class: child.main_class.or(parent.main_class),
        minimum_launcher_version: child
            .minimum_launcher_version
            .or(parent.minimum_launcher_version),
        version_type: child.version_type.or(parent.version_type),
        assets: child.assets.or(parent.assets),
        asset_index: child.asset_index.or(parent.asset_index),
        downloads: merge_optional_maps(parent.downloads, child.downloads),
        libraries: merge_libraries(parent.libraries, child.libraries),
        arguments: merge_arguments(parent.arguments, child.arguments),
        minecraft_arguments: child
            .minecraft_arguments
            .or(parent.minecraft_arguments),
        java_version: child.java_version.or(parent.java_version),
        logging: child.logging.or(parent.logging),
        inherits_from: child.inherits_from.or(parent.inherits_from),
    }
}

fn merge_optional_maps(
    a: Option<HashMap<String, Download>>,
    b: Option<HashMap<String, Download>>,
) -> Option<HashMap<String, Download>> {
    match (a, b) {
        (Some(mut a), Some(b)) => {
            a.extend(b);
            Some(a)
        }
        (a, b) => a.or(b),
    }
}

/// 合并库列表：同名坐标以子版本为准，子版本的库排在后面。
fn merge_libraries(
    parent: Option<Vec<Library>>,
    child: Option<Vec<Library>>,
) -> Option<Vec<Library>> {
    match (parent, child) {
        (Some(mut p), Some(c)) => {
            for lib in c {
                if let Some(existing) = p.iter_mut().find(|l| l.name == lib.name) {
                    *existing = lib;
                } else {
                    p.push(lib);
                }
            }
            Some(p)
        }
        (p, c) => p.or(c),
    }
}

fn merge_arguments(parent: Option<Arguments>, child: Option<Arguments>) -> Option<Arguments> {
    match (parent, child) {
        (Some(p), Some(c)) => Some(Arguments {
            game: p.game.into_iter().chain(c.game).collect(),
            jvm: match (p.jvm, c.jvm) {
                (Some(pj), Some(cj)) => Some(pj.into_iter().chain(cj).collect()),
                (pj, cj) => pj.or(cj),
            },
        }),
        (p, c) => p.or(c),
    }
}
