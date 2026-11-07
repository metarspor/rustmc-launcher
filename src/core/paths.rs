//! .minecraft 目录结构管理。

use std::path::{Path, PathBuf};

/// 启动器所有文件路径的统一入口。
#[derive(Debug, Clone)]
pub struct LauncherPaths {
    /// .minecraft 根目录（游戏目录）
    pub base_dir: PathBuf,
}

impl Default for LauncherPaths {
    fn default() -> Self {
        Self {
            base_dir: default_minecraft_dir(),
        }
    }
}

impl LauncherPaths {
    /// 允许用自定义游戏目录覆盖默认位置。
    #[allow(dead_code)]
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
        }
    }

    /// versions 目录
    pub fn versions_dir(&self) -> PathBuf {
        self.base_dir.join("versions")
    }

    /// 某个版本的目录
    pub fn version_dir(&self, id: &str) -> PathBuf {
        self.versions_dir().join(id)
    }

    /// 版本 JSON
    pub fn version_json(&self, id: &str) -> PathBuf {
        self.version_dir(id).join(format!("{id}.json"))
    }

    /// 版本客户端 jar
    pub fn version_jar(&self, id: &str) -> PathBuf {
        self.version_dir(id).join(format!("{id}.jar"))
    }

    /// libraries 根目录
    pub fn libraries_dir(&self) -> PathBuf {
        self.base_dir.join("libraries")
    }

    /// 某版本的 natives 解压目录
    pub fn natives_dir(&self, id: &str) -> PathBuf {
        self.version_dir(id).join("natives")
    }

    /// assets 根目录
    pub fn assets_dir(&self) -> PathBuf {
        self.base_dir.join("assets")
    }

    /// assets/indexes 目录
    pub fn assets_indexes_dir(&self) -> PathBuf {
        self.assets_dir().join("indexes")
    }

    /// assets/objects 目录
    pub fn assets_objects_dir(&self) -> PathBuf {
        self.assets_dir().join("objects")
    }

    /// 资源索引文件路径
    pub fn asset_index_path(&self, index_id: &str) -> PathBuf {
        self.assets_indexes_dir().join(format!("{index_id}.json"))
    }

    /// 单个资源对象路径（按 hash 前两位分桶）
    pub fn asset_object(&self, hash: &str) -> PathBuf {
        let bucket = hash.get(..2).unwrap_or("xx");
        self.assets_objects_dir()
            .join(bucket)
            .join(hash)
    }

    /// log4j 配置目录
    pub fn log_configs_dir(&self) -> PathBuf {
        self.assets_dir().join("log_configs")
    }

    /// 按 Maven 坐标计算库文件的相对路径。
    /// 坐标格式：`group:artifact:version[:classifier]`
    pub fn library_rel_path(coordinate: &str) -> Option<String> {
        let parts: Vec<&str> = coordinate.split(':').collect();
        if parts.len() < 3 {
            return None;
        }
        let group = parts[0].replace('.', "/");
        let artifact = parts[1];
        let version = parts[2];
        let classifier = parts.get(3).map(|c| format!("-{c}")).unwrap_or_default();
        Some(format!(
            "{group}/{artifact}/{version}/{artifact}-{version}{classifier}.jar"
        ))
    }

    /// 按 Maven 坐标计算库文件的绝对路径。
    #[allow(dead_code)]
    pub fn library_path(&self, coordinate: &str) -> Option<PathBuf> {
        Self::library_rel_path(coordinate)
            .map(|rel| self.libraries_dir().join(rel))
    }

    /// 确保所有必需目录存在。
    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        for dir in [
            self.versions_dir(),
            self.libraries_dir(),
            self.assets_indexes_dir(),
            self.assets_objects_dir(),
            self.log_configs_dir(),
        ] {
            std::fs::create_dir_all(&dir)?;
        }
        Ok(())
    }
}

/// 各平台默认的游戏数据目录（可见目录，不使用隐藏的 `.minecraft`）。
pub fn default_minecraft_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = dirs::data_dir() {
            return appdata.join("RustMC");
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = dirs::home_dir() {
            return home
                .join("Library")
                .join("Application Support")
                .join("RustMC");
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Some(home) = dirs::home_dir() {
            return home.join("RustMC");
        }
    }
    Path::new(".").to_path_buf()
}

/// 官方启动器目录（~/.minecraft），作为首次引导里"兼容旧数据"的预设。
pub fn legacy_minecraft_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = dirs::data_dir() {
            return appdata.join(".minecraft");
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = dirs::home_dir() {
            return home
                .join("Library")
                .join("Application Support")
                .join("minecraft");
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Some(home) = dirs::home_dir() {
            return home.join(".minecraft");
        }
    }
    Path::new(".").to_path_buf()
}

/// 自动检测其他启动器可能使用的 .minecraft 目录（存在且含 versions/ 才返回）。
pub fn detect_external_dirs(current: &Path) -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    let home = dirs::home_dir();

    // 官方 / HMCL / PCL 默认目录
    candidates.push(legacy_minecraft_dir());
    if let Some(h) = &home {
        candidates.push(h.join(".minecraft"));
        candidates.push(h.join("HMCL").join(".minecraft"));
        candidates.push(h.join("PCL").join(".minecraft"));
        candidates.push(h.join("PCL2").join(".minecraft"));
        candidates.push(h.join("Documents").join("PCL").join(".minecraft"));
    }
    #[cfg(target_os = "windows")]
    if let Some(appdata) = dirs::data_dir() {
        candidates.push(appdata.join(".minecraft"));
    }
    #[cfg(target_os = "macos")]
    if let Some(h) = &home {
        candidates.push(
            h.join("Library")
                .join("Application Support")
                .join("minecraft"),
        );
    }

    let mut out = Vec::new();
    for c in candidates {
        if c != current && c.join("versions").is_dir() && !out.contains(&c) {
            out.push(c);
        }
    }
    out
}
