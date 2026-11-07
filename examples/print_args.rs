//! 打印某个版本的完整启动命令（不实际启动），用于验证参数构造。
//!
//! 用法：cargo run --release --example print_args -- [版本ID]

use rustmc_launcher::core::auth::Account;
use rustmc_launcher::core::launch::{self, LaunchOptions};
use rustmc_launcher::core::manifest;
use rustmc_launcher::core::paths::LauncherPaths;
use rustmc_launcher::core::rules::EnvInfo;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let version_id = std::env::args().nth(1).unwrap_or_else(|| "1.16.5".to_string());

    let client = reqwest::Client::new();
    let paths = LauncherPaths::default();
    let env = EnvInfo::current();
    let account = Account::offline("Test");
    let manifest = manifest::fetch_manifest(&client).await?;
    let detail =
        manifest::load_resolved_version(&version_id, &paths, &client, Some(&manifest))
            .await?;

    let opts = LaunchOptions::default();
    let (jvm, main_class, game) =
        launch::build_arguments(&paths, &detail, &account, &env, &opts, &paths.base_dir)?;

    println!("Java: {}", opts.java.display());
    println!("\n# JVM 参数（{} 项）", jvm.len());
    for a in &jvm {
        println!("  {a}");
    }
    println!("\n# 主类\n  {main_class}");
    println!("\n# 游戏参数（{} 项）", game.len());
    for a in &game {
        println!("  {a}");
    }

    // 检查是否残留未替换的占位符
    let all = [jvm.join(" "), game.join(" ")];
    let unresolved: Vec<&str> = all
        .iter()
        .flat_map(|s| s.split_whitespace())
        .filter(|t| t.contains("${"))
        .collect();
    if unresolved.is_empty() {
        println!("\n✓ 所有占位符均已替换，无残留。");
    } else {
        println!("\n[警告] 存在未替换的占位符：{unresolved:?}");
    }
    Ok(())
}
