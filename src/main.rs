// Windows 下以 GUI 子系统运行，双击不弹控制台黑窗（其它平台忽略此属性）。
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
//! RustMC Launcher —— 轻量、快速、稳定的 Minecraft 启动器（图形界面）。

mod gui;

use rustmc_launcher::core::java;

/// 前置环境校验：操作系统、Java（打印到启动终端，仅作提示）。
fn preflight() {
    let os_label = if cfg!(target_os = "linux") {
        "Linux"
    } else if cfg!(target_os = "macos") {
        "macOS"
    } else if cfg!(target_os = "windows") {
        "Windows"
    } else {
        "未知系统"
    };
    println!("[*] RustMC Launcher 启动中…");
    println!("[*] 操作系统：{os_label}（{}）", std::env::consts::ARCH);

    let javas = java::detect_all();
    if javas.is_empty() {
        println!(
            "[!] 未检测到 Java，启动器在启动游戏时会提示所需版本\n    （如 sudo apt install openjdk-17-jre）"
        );
    } else {
        for j in &javas {
            println!("[+] 找到 {}", j.display());
        }
    }
}

fn main() -> eframe::Result<()> {
    preflight();

    // 后台 tokio 运行时（move 到 GUI 中保持存活）
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("无法创建 tokio 运行时");

    // 窗口标题栏 / 任务栏图标（32x32 RGBA，由 tools/make_icons.py 生成并内嵌）。
    let icon_bytes = include_bytes!("../assets/icon/window-32.rgba");
    let icon = egui::IconData {
        rgba: icon_bytes.to_vec(),
        width: 32,
        height: 32,
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([980.0, 660.0])
            .with_min_inner_size([820.0, 580.0])
            .with_title("RustMC Launcher")
            .with_icon(icon),
        ..Default::default()
    };

    eframe::run_native(
        "RustMC Launcher",
        options,
        Box::new(|cc| Ok(Box::new(gui::GuiApp::new(rt, &cc.egui_ctx)))),
    )
}
