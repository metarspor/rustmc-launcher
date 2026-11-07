//! 自动优化：读取系统内存给出推荐分配，低配机预设 JVM/游戏参数。

/// 读取系统总物理内存（MB）。读不到时返回 None。
pub fn total_memory_mb() -> Option<u64> {
    // Linux: /proc/meminfo
    #[cfg(target_os = "linux")]
    {
        if let Ok(text) = std::fs::read_to_string("/proc/meminfo") {
            for line in text.lines() {
                if let Some(rest) = line.strip_prefix("MemTotal:") {
                    let kb: u64 = rest
                        .split_whitespace()
                        .next()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                    return Some(kb / 1024);
                }
            }
        }
    }
    // macOS: sysctl hw.memsize
    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = std::process::Command::new("sysctl")
            .arg("-n")
            .arg("hw.memsize")
            .output()
        {
            if let Ok(s) = String::from_utf8(out.stdout) {
                let bytes: u64 = s.trim().parse().ok()?;
                return Some(bytes / 1024 / 1024);
            }
        }
    }
    // Windows: GetPhysicallyInstalledSystemMemory via kernel32（用简单方式：wmic）
    #[cfg(target_os = "windows")]
    {
        if let Ok(out) = std::process::Command::new("wmic")
            .args(["ComputerSystem", "get", "TotalPhysicalMemory"])
            .output()
        {
            let text = String::from_utf8_lossy(&out.stdout);
            for line in text.lines() {
                if let Ok(bytes) = line.trim().parse::<u64>() {
                    return Some(bytes / 1024 / 1024);
                }
            }
        }
    }
    None
}

/// 根据总内存建议 (min_mb, max_mb)。
///
/// 原则：
/// - 总内存 ≤ 2G：给 MC 512M（系统都紧巴巴）
/// - 总内存 4G：给 1G（留 3G 给系统和浏览器）
/// - 总内存 8G：给 2.5G
/// - 总内存 ≥ 16G：给 4G（再多 MC 也吃不下）
pub fn suggest_memory(total_mb: u64) -> (u32, u32) {
    let max = match total_mb {
        0..=2048 => 512,
        2049..=4096 => 1024,
        4097..=8192 => 2560,
        8193..=16384 => 4096,
        _ => 4096,
    };
    let min = (max / 2).clamp(256, 1024);
    (min, max)
}

/// 低配机 JVM 参数：压低 GC 开销、禁用多余特性。
pub fn low_spec_jvm_args() -> Vec<String> {
    vec![
        "-XX:+UseG1GC".into(),
        "-XX:+UnlockExperimentalVMOptions".into(),
        "-XX:+UseStringDeduplication".into(),
        "-XX:G1NewSizePercent=20".into(),
        "-XX:G1ReservePercent=20".into(),
        "-XX:MaxGCPauseMillis=50".into(),
        "-XX:G1HeapRegionSize=4M".into(),
        "-XX:+DisableExplicitGC".into(),
        "-Dfile.encoding=UTF-8".into(),
    ]
}

/// 低配机游戏参数：锁 30 帧、降视野。
pub fn low_spec_game_args() -> Vec<String> {
    vec![
        "--maxFramerate".into(),
        "30".into(),
        "--fov".into(),
        "60".into(),
    ]
}
