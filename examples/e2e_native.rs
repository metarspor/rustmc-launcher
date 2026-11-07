//! 端到端 native 验证：只下载库（跳过 assets），解压，导出 classpath。
//! 随后用 Java 真实加载 LWJGL 验证。

use rustmc_launcher::core::download::{self, DownloadEvent};
use rustmc_launcher::core::auth::Account;
use rustmc_launcher::core::launch;
use rustmc_launcher::core::manifest;
use rustmc_launcher::core::paths::LauncherPaths;
use rustmc_launcher::core::rules::EnvInfo;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let version_id = std::env::args().nth(1).unwrap_or_else(|| "26.4-snapshot-5".to_string());
    let tmp = std::env::temp_dir().join("fixverify");
    let _ = std::fs::remove_dir_all(&tmp);

    let client = reqwest::Client::builder().user_agent("RustMC-Launcher/0.1").build()?;
    let paths = LauncherPaths::new(&tmp);
    paths.ensure_dirs()?;
    let env = EnvInfo::current();

    let manifest = manifest::fetch_manifest(&client).await?;
    let detail =
        manifest::load_resolved_version(&version_id, &paths, &client, Some(&manifest)).await?;

    let (tasks, _) = download::collect_tasks(&client, &paths, &detail, &env).await?;
    // 只保留库与客户端 jar，跳过 assets（路径含 /assets/）
    let lib_tasks: Vec<_> = tasks
        .into_iter()
        .filter(|t| !t.target.to_string_lossy().contains("/assets/"))
        .collect();
    println!("库文件任务：{}", lib_tasks.len());

    let (tx, mut rx) = mpsc::unbounded_channel::<DownloadEvent>();
    let worker = {
        let client = client.clone();
        tokio::spawn(async move { download::run(client, lib_tasks, tx, 16).await })
    };
    while (rx.recv().await).is_some() {}
    worker.await??;

    download::extract_natives(&paths, &detail, &env)?;

    let account = Account::offline("Test");
    let opts = launch::LaunchOptions::default();
    let (jvm, _, _) =
        launch::build_arguments(&paths, &detail, &account, &env, &opts, &paths.base_dir)?;

    // 导出 classpath 与完整 JVM 参数
    let cp_idx = jvm.iter().position(|a| a == "-cp").unwrap();
    std::fs::write(tmp.join("cp.txt"), &jvm[cp_idx + 1])?;
    std::fs::write(
        tmp.join("jvm.txt"),
        jvm.iter().filter(|a| a.starts_with("-D")).cloned().collect::<Vec<_>>().join("\n"),
    )?;
    println!("classpath 已导出：{}/cp.txt", tmp.display());
    println!("✓ 库下载与解压完成");
    Ok(())
}
