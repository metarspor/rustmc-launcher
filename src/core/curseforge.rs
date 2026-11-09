//! CurseForge API 搜索与下载。
//!
//! 使用国内开源镜像 mod.mcimirror.top（https://github.com/mcmod-info-mirror/mcim-rust-api），
//! 无需 API Key，国内访问快速。

use anyhow::Result;
use serde::Deserialize;

use crate::core::content::{ContentKind, SearchHit};

const BASE: &str = "https://mod.mcimirror.top/curseforge";

/// CurseForge 分类 ID（gameId=432 = Minecraft）。
fn class_id(kind: ContentKind) -> u32 {
    match kind {
        ContentKind::Mod => 6,
        ContentKind::ResourcePack => 4471,
        ContentKind::ShaderPack => 6552,
        ContentKind::World => 12,
        ContentKind::Modpack => 4472,
    }
}

#[derive(Debug, Deserialize)]
struct CfSearchResponse {
    data: Vec<CfMod>,
}

#[derive(Debug, Deserialize)]
struct CfMod {
    id: u64,
    name: String,
    summary: String,
    authors: Vec<CfAuthor>,
    downloadCount: f64,
    logo: Option<CfLogo>,
}

#[derive(Debug, Deserialize)]
struct CfAuthor {
    name: String,
}

#[derive(Debug, Deserialize)]
struct CfLogo {
    url: String,
}

/// 搜索 CurseForge（通过国内镜像，无需 API Key）。
pub async fn search(
    client: &reqwest::Client,
    _api_key: &str,
    query: &str,
    kind: ContentKind,
    mc_version: Option<&str>,
    limit: u32,
) -> Result<Vec<SearchHit>> {
    let mut req = client
        .get(format!("{BASE}/v1/mods/search"))
        .header("User-Agent", "RustMC-Launcher/0.1")
        .query(&[
            ("gameId", "432".to_string()),
            ("classId", class_id(kind).to_string()),
            ("searchFilter", query.to_string()),
            ("sortField", "2".to_string()), // 2 = Popularity
            ("sortOrder", "desc".to_string()),
            ("pageSize", limit.to_string()),
            ("index", "0".to_string()),
        ]);
    if let Some(mc) = mc_version {
        req = req.query(&[("gameVersion", mc.to_string())]);
    }
    let resp: CfSearchResponse = req.send().await?.error_for_status()?.json().await?;

    let hits = resp
        .data
        .into_iter()
        .map(|m| SearchHit {
            project_id: format!("cf:{}", m.id),
            slug: None,
            title: m.name,
            description: m.summary,
            author: m.authors.first().map(|a| a.name.clone()).unwrap_or_default(),
            downloads: m.downloadCount as u64,
            icon_url: m.logo.map(|l| l.url),
            project_type: kind.modrinth_type().to_string(),
            display_categories: Vec::new(),
        })
        .collect();
    Ok(hits)
}
