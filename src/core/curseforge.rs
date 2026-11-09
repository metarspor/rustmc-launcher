//! CurseForge API 搜索与下载。
//!
//! 需要用户在设置里填入 CurseForge API Key（https://console.curseforge.com/）。
//! 搜索结果复用 content::SearchHit 结构，project_id 前缀加 "cf:" 以区分来源。

use anyhow::Result;
use serde::Deserialize;

use crate::core::content::{ContentKind, SearchHit};

const BASE: &str = "https://api.curseforge.com";

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

/// 搜索 CurseForge，结果转成通用 SearchHit。
pub async fn search(
    client: &reqwest::Client,
    api_key: &str,
    query: &str,
    kind: ContentKind,
    mc_version: Option<&str>,
    limit: u32,
) -> Result<Vec<SearchHit>> {
    let mut req = client
        .get(format!("{BASE}/v1/mods/search"))
        .header("x-api-key", api_key)
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
            // 用 cf: 前缀区分 CurseForge 项目
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
