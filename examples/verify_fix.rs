//! 验证 native 修复：26.x 的 classpath 必须包含所有 native jar，
//! 且 java.library.path 指向 natives/java。不落盘库文件，只构造参数。

use rustmc_launcher::core::auth::Account;
use rustmc_launcher::core::launch::{self, LaunchOptions};
use rustmc_launcher::core::manifest;
use rustmc_launcher::core::paths::LauncherPaths;
use rustmc_launcher::core::rules::EnvInfo;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let version_id = std::env::args().nth(1).unwrap_or_else(|| "26.4-snapshot-5".to_string());
    let tmp = std::env::temp_dir().join("fixverify");
    let _ = std::fs::remove_dir_all(&tmp);

    let client = reqwest::Client::new();
    let paths = LauncherPaths::new(&tmp);
    let env = EnvInfo::current();
    let account = Account::offline("Test");
    let manifest = manifest::fetch_manifest(&client).await?;
    let detail =
        manifest::load_resolved_version(&version_id, &paths, &client, Some(&manifest)).await?;

    let opts = LaunchOptions::default();
    let (jvm, _main, _game) =
        launch::build_arguments(&paths, &detail, &account, &env, &opts, &paths.base_dir)?;

    // 提取 classpath
    let cp_idx = jvm.iter().position(|a| a == "-cp").unwrap();
    let cp = &jvm[cp_idx + 1];
    let entries: Vec<&str> = cp.split(':').collect();

    // 找 java.library.path
    let jlp = jvm
        .iter()
        .find(|a| a.starts_with("-Djava.library.path="))
        .cloned()
        .unwrap_or_default();

    let native_entries: Vec<&&str> = entries.iter().filter(|e| e.contains("natives-")).collect();
    let lwjgl_native: Vec<&&str> = native_entries.iter().filter(|e| e.contains("lwjgl")).cloned().collect();
    let jtracy: Vec<&&str> = native_entries.iter().filter(|e| e.contains("jtracy")).cloned().collect();

    println!("版本：{version_id}");
    println!("classpath 总条目：{}", entries.len());
    println!("native jar 条目：{}", native_entries.len());
    println!("  LWJGL native：{}", lwjgl_native.len());
    println!("  jtracy native：{}", jtracy.len());
    println!("java.library.path：{jlp}");

    let mut ok = true;
    if native_entries.len() < 11 {
        println!("[失败] 期望至少 11 个 native jar，实际 {}", native_entries.len());
        ok = false;
    }
    if !jlp.ends_with("/natives/java") {
        println!("[失败] java.library.path 未指向 natives/java：{jlp}");
        ok = false;
    }
    if ok {
        println!("\n✓ 修复验证通过：native jar 已在 classpath，java.library.path 正确。");
    }
    Ok(())
}
