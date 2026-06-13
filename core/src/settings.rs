use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const SCHEMA_JSONC: &str = include_str!("settings.schema.jsonc");

fn default_schema_path() -> String {
    "https://raw.githubusercontent.com/vima-tech/Innate/main/settings.schema.jsonc".to_string()
}

// ---------------------------------------------------------------------------
// Top-level Settings
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// JSON Schema reference — always present; written alongside settings.json.
    #[serde(rename = "$schema", default = "default_schema_path")]
    pub schema: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm: Option<LlmConfig>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<EmbeddingConfig>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon: Option<DaemonConfig>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup: Option<BackupConfig>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema: default_schema_path(),
            llm: None,
            embedding: None,
            daemon: None,
            backup: None,
        }
    }
}

// ---------------------------------------------------------------------------
// LLM (generative) config — used by LlmDistiller
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    /// "openai" | "anthropic"
    pub provider: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,

    pub model_id: String,

    /// API key (env var override: INNATE_LLM_API_KEY, OPENAI_API_KEY, ANTHROPIC_API_KEY)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

impl LlmConfig {
    /// Resolved API key: settings file → env var fallback.
    pub fn resolved_api_key(&self) -> Option<String> {
        if let Some(ref k) = self.api_key {
            if !k.is_empty() {
                return Some(k.clone());
            }
        }
        // Generic override
        if let Ok(k) = std::env::var("INNATE_LLM_API_KEY") {
            if !k.is_empty() {
                return Some(k);
            }
        }
        match self.provider.as_str() {
            "anthropic" => std::env::var("ANTHROPIC_API_KEY")
                .ok()
                .filter(|k| !k.is_empty()),
            _ => std::env::var("OPENAI_API_KEY")
                .ok()
                .filter(|k| !k.is_empty()),
        }
    }

    pub fn resolved_base_url(&self) -> String {
        if let Some(ref u) = self.base_url {
            if !u.is_empty() {
                return u.trim_end_matches('/').to_string();
            }
        }
        match self.provider.as_str() {
            "anthropic" => "https://api.anthropic.com".to_string(),
            _ => "https://api.openai.com/v1".to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Embedding config — used by LlmEmbeddingProvider
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    /// Only "openai" format is supported (Anthropic has no embedding API).
    #[serde(default = "default_openai")]
    pub provider: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,

    pub model_id: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,

    /// Embedding output dimension (model-specific; defaults to 1536 for text-embedding-3-small).
    #[serde(default = "default_embed_dim")]
    pub dim: usize,
}

fn default_openai() -> String {
    "openai".to_string()
}

fn default_embed_dim() -> usize {
    1536
}

impl EmbeddingConfig {
    pub fn resolved_api_key(&self) -> Option<String> {
        if let Some(ref k) = self.api_key {
            if !k.is_empty() {
                return Some(k.clone());
            }
        }
        if let Ok(k) = std::env::var("INNATE_LLM_API_KEY") {
            if !k.is_empty() {
                return Some(k);
            }
        }
        std::env::var("OPENAI_API_KEY")
            .ok()
            .filter(|k| !k.is_empty())
    }

    pub fn resolved_base_url(&self) -> String {
        self.base_url
            .as_deref()
            .filter(|u| !u.is_empty())
            .map(|u| u.trim_end_matches('/').to_string())
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string())
    }
}

// ---------------------------------------------------------------------------
// Daemon config
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    /// Directories the daemon watches for .log files.
    #[serde(default)]
    pub watch_dirs: Vec<String>,

    /// Automatically spawn the daemon when the MCP server starts (default: true).
    #[serde(default = "default_true")]
    pub auto_start: bool,
}

fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------------------
// Backup config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupConfig {
    /// Master switch — backup is disabled by default. Set to true to enable.
    #[serde(default)]
    pub enable: bool,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r2: Option<R2Config>,

    /// Auto-backup interval in hours (default: 24).
    #[serde(default = "default_backup_interval_hours")]
    pub auto_backup_interval_hours: u64,

    /// Delete backups older than this many days (default: 60).
    #[serde(default = "default_retention_days")]
    pub retention_days: u64,

    /// Always keep at least this many backup files regardless of age (default: 5).
    #[serde(default = "default_min_backups")]
    pub min_backups: usize,
}

impl Default for BackupConfig {
    fn default() -> Self {
        Self {
            enable: false,
            r2: None,
            auto_backup_interval_hours: default_backup_interval_hours(),
            retention_days: default_retention_days(),
            min_backups: default_min_backups(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct R2Config {
    /// Cloudflare account ID (found in the R2 dashboard URL).
    pub account_id: String,

    /// R2 bucket name.
    pub bucket: String,

    /// R2 API token access key ID. Env override: INNATE_R2_ACCESS_KEY_ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_key_id: Option<String>,

    /// R2 API token secret access key. Env override: INNATE_R2_SECRET_ACCESS_KEY.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_access_key: Option<String>,

    /// Optional key prefix (e.g. "innate/"). Default: "".
    #[serde(default)]
    pub prefix: String,
}

impl R2Config {
    pub fn resolved_access_key_id(&self) -> Option<String> {
        if let Some(ref k) = self.access_key_id {
            if !k.is_empty() {
                return Some(k.clone());
            }
        }
        std::env::var("INNATE_R2_ACCESS_KEY_ID")
            .ok()
            .filter(|k| !k.is_empty())
    }

    pub fn resolved_secret_access_key(&self) -> Option<String> {
        if let Some(ref k) = self.secret_access_key {
            if !k.is_empty() {
                return Some(k.clone());
            }
        }
        std::env::var("INNATE_R2_SECRET_ACCESS_KEY")
            .ok()
            .filter(|k| !k.is_empty())
    }
}

fn default_backup_interval_hours() -> u64 {
    24
}

fn default_retention_days() -> u64 {
    60
}

fn default_min_backups() -> usize {
    5
}

// ---------------------------------------------------------------------------
// Load / save
// ---------------------------------------------------------------------------

/// Returns `~/.innate/settings.json`.
pub fn settings_path() -> PathBuf {
    crate::paths::settings_path()
}

/// Load settings from `~/.innate/settings.json`. Returns `Settings::default()` if absent.
pub fn load() -> Settings {
    let path = settings_path();
    load_from(&path)
}

pub fn load_from(path: &Path) -> Settings {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Settings::default();
    };
    // A present-but-unparseable settings file must not silently degrade to
    // defaults (which would drop the user's LLM/embedding config and fall back
    // to the Dummy provider). Warn loudly so the misconfiguration is visible.
    match serde_json::from_str(&text) {
        Ok(settings) => settings,
        Err(e) => {
            eprintln!(
                "innate: WARNING — {} exists but could not be parsed ({e}); \
                 ignoring it and using defaults. LLM/embedding/backup settings \
                 will NOT take effect until the file is fixed.",
                path.display()
            );
            Settings::default()
        }
    }
}

/// Write settings to `~/.innate/settings.json` with mode 0600.
pub fn save(settings: &Settings) -> anyhow::Result<()> {
    let path = settings_path();
    save_to(settings, &path)
}

pub fn save_to(settings: &Settings, path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        // Write the schema file alongside settings.json so $schema resolves locally.
        let schema_path = parent.join("settings.schema.jsonc");
        let _ = std::fs::write(&schema_path, SCHEMA_JSONC);
    }
    let json = serde_json::to_string_pretty(settings)?;
    std::fs::write(path, &json)?;
    // 0600 on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Expand `~` at the start of a path string to the home directory.
pub fn expand_tilde(path: &str) -> String {
    if path.starts_with("~/") || path == "~" {
        let home = dirs_next::home_dir()
            .map(|h| h.display().to_string())
            .unwrap_or_default();
        path.replacen('~', &home, 1)
    } else {
        path.to_string()
    }
}

/// Return expanded watch directories from daemon config.
pub fn resolved_watch_dirs(settings: &Settings) -> Vec<String> {
    settings
        .daemon
        .as_ref()
        .map(|d| d.watch_dirs.iter().map(|p| expand_tilde(p)).collect())
        .unwrap_or_default()
}
