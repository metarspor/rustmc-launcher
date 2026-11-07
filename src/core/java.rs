//! Java 运行环境检测与版本兼容性判断。

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::core::manifest::VersionDetail;

/// 一个可用的 Java 运行时。
#[derive(Debug, Clone)]
pub struct JavaInfo {
    /// java 可执行文件路径
    pub path: PathBuf,
    /// 原始版本字符串，如 `11.0.32`
    pub raw_version: String,
    /// 主版本号，如 8 / 11 / 17 / 21
    pub major: u32,
}

impl JavaInfo {
    pub fn display(&self) -> String {
        format!("Java {} ({})", self.major, self.raw_version)
    }
}

/// 探测系统中所有可用的 Java。
pub fn detect_all() -> Vec<JavaInfo> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    // 1. JAVA_HOME
    if let Ok(java_home) = std::env::var("JAVA_HOME") {
        candidates.push(PathBuf::from(java_home).join("bin").join(java_exe_name()));
    }

    // 2. PATH 中的 java
    if let Some(path) = which(java_exe_name()) {
        candidates.push(path);
    }

    // 3. Linux 常见安装目录
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let roots = if cfg!(target_os = "macos") {
            vec!["/Library/Java/JavaVirtualMachines".to_string()]
        } else {
            vec![
                "/usr/lib/jvm".to_string(),
                "/usr/java".to_string(),
                "/opt/java".to_string(),
            ]
        };
        for root in roots {
            if let Ok(entries) = std::fs::read_dir(root) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    candidates.push(p.join("bin").join(java_exe_name()));
                    // macOS 的 bundle 结构
                    candidates.push(
                        p.join("Contents")
                            .join("Home")
                            .join("bin")
                            .join(java_exe_name()),
                    );
                }
            }
        }
    }

    // Windows 常见目录
    #[cfg(target_os = "windows")]
    {
        if let Ok(program_files) = std::env::var("ProgramFiles") {
            candidates.push(
                PathBuf::from(program_files)
                    .join("Java")
                    .join("bin")
                    .join("java.exe"),
            );
        }
    }

    // 验证可执行并解析版本；按 canonicalize 后的真实路径去重
    // （/usr/bin/java、/etc/alternatives、/usr/lib/jvm 常指向同一文件）
    let mut found: Vec<JavaInfo> = Vec::new();
    for candidate in candidates {
        if let Ok(info) = probe(&candidate) {
            let real = info
                .path
                .canonicalize()
                .unwrap_or_else(|_| info.path.clone());
            let duplicate = found.iter().any(|f| {
                f.path.canonicalize().unwrap_or_else(|_| f.path.clone()) == real
            });
            if !duplicate {
                found.push(info);
            }
        }
    }
    found.sort_by_key(|j| j.major);
    found
}

/// 验证某个 java 可执行文件并解析版本。
pub fn probe(java_path: &Path) -> Result<JavaInfo> {
    if !java_path.exists() {
        bail!("java 不存在：{}", java_path.display());
    }
    let output = Command::new(java_path)
        .arg("-version")
        .output()
        .with_context(|| format!("无法执行 {}", java_path.display()))?;

    // java -version 的信息输出在 stderr
    let text = String::from_utf8_lossy(&output.stderr);
    let raw_version = parse_version_string(&text)
        .ok_or_else(|| anyhow::anyhow!("无法解析 Java 版本：{text}"))?;
    let major = major_version(&raw_version);

    Ok(JavaInfo {
        path: java_path.to_path_buf(),
        raw_version,
        major,
    })
}

/// 从 `java -version` 输出中提取引号内的版本号。
fn parse_version_string(text: &str) -> Option<String> {
    let start = text.find('"')? + 1;
    let rest = &text[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// 把版本字符串换算为主版本号。
/// `1.8.0_322` -> 8，`11.0.32` -> 11，`17` -> 17。
pub fn major_version(raw: &str) -> u32 {
    let mut parts = raw.split(['.', '_', '-']);
    let first: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    if first == 1 {
        // 1.7 / 1.8 这种旧编号
        parts.next().and_then(|s| s.parse().ok()).unwrap_or(first)
    } else {
        first
    }
}

/// 根据版本 JSON 确定所需的 Java 主版本。
pub fn required_major(detail: &VersionDetail) -> u32 {
    detail
        .java_version
        .as_ref()
        .map(|j| j.major_version)
        .unwrap_or(8)
}

/// 从已探测的 Java 中挑选满足要求的最高兼容版本。
pub fn pick_compatible(available: &[JavaInfo], required_major: u32) -> Option<JavaInfo> {
    available
        .iter()
        .filter(|j| j.major >= required_major)
        .max_by_key(|j| j.major)
        .cloned()
        .or_else(|| available.first().cloned())
}

fn java_exe_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "java.exe"
    } else {
        "java"
    }
}

/// 极简 `which`：在 PATH 中查找可执行文件。
fn which(exe: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(exe);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}
