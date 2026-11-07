//! Mojang 版本 JSON 中的 rules 解析。
//!
//! 规则按顺序求值，最后一条匹配的规则决定结果；
//! 没有任何规则时默认允许。

use std::collections::HashMap;

use crate::core::manifest::Rule;

/// 当前运行环境的信息。
#[derive(Debug, Clone)]
pub struct EnvInfo {
    /// Mojang 命名的操作系统：`windows` / `osx` / `linux`
    pub os_name: &'static str,
    /// 操作系统版本（规则中一般不强制）
    pub os_version: String,
    /// 架构（`x86` / `x64` / `arm64`）
    pub arch: &'static str,
    /// 特性开关（demo、quickPlay 等），默认全部关闭
    pub features: HashMap<String, bool>,
}

impl EnvInfo {
    /// 采集当前环境。
    pub fn current() -> Self {
        let (os_name, arch) = current_os_and_arch();
        Self {
            os_name,
            os_version: os_release_version(),
            arch,
            features: HashMap::new(),
        }
    }
}

/// 对一组规则求值，返回是否允许。
///
/// 语义：
/// - 规则为空 → 允许（在 `allowed()` 中处理）
/// - 有规则时，**初始状态为拒绝**，按顺序由匹配的规则覆盖；
///   没有任何规则匹配 → 拒绝。
pub fn evaluate(rules: &[Rule], env: &EnvInfo) -> bool {
    let mut allowed = false;
    for rule in rules {
        if rule_matches(rule, env) {
            allowed = rule.action == "allow";
        }
    }
    allowed
}

/// 没有规则时的便捷判断：规则缺失或为空 => 允许。
pub fn allowed(rules: Option<&Vec<Rule>>, env: &EnvInfo) -> bool {
    match rules {
        Some(rules) if !rules.is_empty() => evaluate(rules, env),
        _ => true,
    }
}

fn rule_matches(rule: &Rule, env: &EnvInfo) -> bool {
    // OS 条件
    if let Some(os) = &rule.os {
        if let Some(name) = &os.name
            && name != env.os_name {
                return false;
            }
        if let Some(version_pattern) = &os.version {
            // Mojang 用正则（如 Windows 版本），这里只做包含匹配即可覆盖现有规则
            if regex_like_match(version_pattern, &env.os_version).is_none() {
                return false;
            }
        }
        if let Some(arch) = &os.arch
            && arch != env.arch {
                return false;
            }
    }

    // features 条件：所有给定特性都必须与环境一致
    if let Some(features) = &rule.features {
        for (key, expected) in features {
            let actual = env.features.get(key).copied().unwrap_or(false);
            if actual != *expected {
                return false;
            }
        }
    }

    true
}

/// 极简正则匹配：Mojang 规则里出现的形式如 `^10\.`，
/// 这里支持 `^` 前缀与 `.` 转义的常见用法，避免引入完整正则库。
fn regex_like_match(pattern: &str, text: &str) -> Option<()> {
    let anchored_start = pattern.starts_with('^');
    let p = pattern.trim_start_matches('^');
    // 把 "\." 当作字面 '.'，其余字符原样比较（覆盖 Mojang 现有规则）
    let mut p_chars = p.chars();
    let mut t_chars = text.chars();
    let mut matched_any = false;

    loop {
        match p_chars.next() {
            Some('\\') => {
                let next = p_chars.next()?;
                let t = t_chars.next()?;
                if t != next {
                    if anchored_start {
                        return None;
                    }
                    return regex_like_match(pattern, &text[1..]);
                }
            }
            Some(c) => {
                let t = t_chars.next()?;
                if c != '.' && c != t {
                    if anchored_start {
                        return None;
                    }
                    // 非锚定从头滑动一位
                    return regex_like_match(pattern, &text[1..]);
                }
            }
            None => break,
        }
        matched_any = true;
    }
    if matched_any || p.is_empty() {
        Some(())
    } else {
        None
    }
}

/// 返回 (Mojang OS 名称, 架构)。
pub fn current_os_and_arch() -> (&'static str, &'static str) {
    let os_name = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "osx"
    } else {
        "linux"
    };

    let arch = if cfg!(target_arch = "x86_64") {
        "x64"
    } else if cfg!(target_arch = "x86") {
        "x86"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "unknown"
    };

    (os_name, arch)
}

fn os_release_version() -> String {
    // 规则几乎不依赖 Linux 版本，失败时返回空串即可
    std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default()
}
