//! 端到端示例：解析版本 → 完整下载 → 解压 natives → 验证启动参数。
//!
//! 用法：
//!   cargo run --release --example prepare -- [版本ID]
//!   cargo run --release --example prepare -- 1.16.5

use rustmc_launcher::core::download::{self, DownloadEvent};
use rustmc_launcher::core::java;
use rustmc_launcher::core::manifest;
use rustmc_launcher::core::paths::LauncherPaths;
use rustmc_launcher::core::rules::EnvInfo;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let version_id = std::env::args().nth(1).unwrap_or_else(|| "1.16.5".to_string());

    let client = reqwest::Client::builder()
        .user_agent("RustMC-Launcher/0.1")
        .build()?;
    let paths = LauncherPaths::default();
    paths.ensure_dirs()?;
    let env = EnvInfo::current();

    println!("[1/5] 加载版本清单…");
    let manifest = manifest::fetch_manifest(&client).await?;

    println!("[2/5] 解析版本 {version_id}（含继承合并）…");
    let detail =
        manifest::load_resolved_version(&version_id, &paths, &client, Some(&manifest))
            .await?;

    println!("[3/5] 收集下载任务…");
    let (tasks, _) =
        download::collect_tasks(&client, &paths, &detail, &env).await?;
    println!("      共 {} 个文件任务", tasks.len());

    // 下载 + 进度转发
    let (tx, mut rx) = mpsc::unbounded_channel::<DownloadEvent>();
    let worker = {
        let client = client.clone();
        tokio::spawn(async move { download::run(client, tasks, tx, 16).await })
    };

    let mut last = String::new();
    while let Some(ev) = rx.recv().await {
        if let DownloadEvent::FileDone {
            completed,
            total,
            finished_bytes,
            total_bytes,
            ..
        } = ev
        {
            let line = format!(
                "      进度 {completed}/{total}  {:.1}/{:.1} MB",
                finished_bytes as f64 / 1_048_576.0,
                total_bytes as f64 / 1_048_576.0
            );
            if line != last {
                println!("\r{line}");
                last = line;
            }
        }
    }
    worker.await??;

    println!("[4/5] 解压原生库…");
    download::extract_natives(&paths, &detail, &env)?;
    let natives = paths.natives_dir(&version_id);
    let mut native_files: Vec<String> = std::fs::read_dir(&natives)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    native_files.sort();
    println!("      natives 目录：{}", natives.display());
    for f in native_files {
        println!("        - {f}");
    }

    println!("[5/5] Java 兼容性检查…");
    let required = java::required_major(&detail);
    let available = java::detect_all();
    match java::pick_compatible(&available, required) {
        Some(j) => println!("      选择 {}（需要 Java {required}）", j.display()),
        None => println!("      [警告] 未找到兼容 Java（需要 Java {required}）"),
    }

    println!("\n✓ 版本 {version_id} 准备完成，可在 TUI 中启动。");
    Ok(())
}
