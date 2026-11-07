//! 真实 Forge 安装器的原生安装测试（仅用于开发验证，不随正式包分发）。
//!
//! 运行：
//!   cargo run --example forge_test -- /tmp/forge-installer.jar /tmp/forge-native-test 1.20.1

use std::path::PathBuf;

use rustmc_launcher::core::installer;
use rustmc_launcher::core::java;
use rustmc_launcher::core::modloaders::InstallMessage;
use rustmc_launcher::core::paths::LauncherPaths;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let installer_path = PathBuf::from(args.get(1).cloned().unwrap_or_else(|| "/tmp/forge-installer.jar".into()));
    let game_dir = PathBuf::from(args.get(2).cloned().unwrap_or_else(|| "/tmp/forge-native-test".into()));
    let mc = args.get(3).map(|s| s.as_str()).unwrap_or("1.20.1");

    // 默认全新安装；设置 KEEP=1 时复用已有目录以验证幂等性。
    if std::env::var("KEEP").is_err() {
        let _ = std::fs::remove_dir_all(&game_dir);
    }
    let paths = LauncherPaths::new(&game_dir);

    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap();

    let javas = java::detect_all();
    println!("检测到 Java：");
    for j in &javas {
        println!("  - {} ({})", j.major, j.path.display());
    }
    let java = java::pick_compatible(&javas, 17)
        .or_else(|| javas.first().cloned())
        .expect("系统未找到 Java");
    println!("使用：{} ({})", java.major, java.path.display());

    println!("新式安装器？{}", installer::is_new_installer(&installer_path));

    let (tx, mut rx) = mpsc::unbounded_channel::<InstallMessage>();
    let printer = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            match m {
                InstallMessage::Status(s) => println!("[状态] {s}"),
                InstallMessage::Download(e) => {
                    use rustmc_launcher::core::download::DownloadEvent;
                    if let DownloadEvent::FileDone { completed, total, name, .. } = e {
                        if completed % 10 == 0 || completed == total {
                            println!("[下载] {completed}/{total} {name}");
                        }
                    }
                }
            }
        }
    });

    let result = installer::install(&client, &paths, &installer_path, mc, &java, 16, &tx).await;
    drop(tx);
    let _ = printer.await;

    match result {
        Ok(id) => {
            println!("✅ 安装成功，版本 id = {id}");
            let vj = paths.version_json(&id);
            println!("版本 JSON：{} 存在={}", vj.display(), vj.exists());
        }
        Err(e) => {
            println!("❌ 安装失败：{e:#}");
            std::process::exit(1);
        }
    }
}
