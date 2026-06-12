//! Cloudflare R2 backup for the Innate knowledge database.
//!
//! Uses S3-compatible API with AWS Signature V4.
//! R2 endpoint: https://{account_id}.r2.cloudflarestorage.com
//! Region for signing: "auto"

use std::path::Path;

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

use crate::settings::{BackupConfig, R2Config};

type HmacSha256 = Hmac<Sha256>;

// ── Backup state (local cache of last backup time) ───────────────────────────

fn backup_state_path() -> std::path::PathBuf {
    dirs_next::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".innate")
        .join("backup_state.json")
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct BackupState {
    pub last_backup_at: Option<String>,
    pub last_backup_key: Option<String>,
}

fn load_state() -> BackupState {
    let Ok(text) = std::fs::read_to_string(backup_state_path()) else {
        return BackupState::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

fn save_state(state: &BackupState) -> anyhow::Result<()> {
    let path = backup_state_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(state)?)?;
    Ok(())
}

// ── Public result types ───────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize)]
pub struct BackupInfo {
    pub key: String,
    pub last_modified: String,
    pub size_bytes: u64,
}

#[derive(Debug, serde::Serialize)]
pub struct PruneResult {
    pub deleted: Vec<String>,
    pub kept: usize,
    /// Backups older than retention_days that were NOT deleted to honour min_backups.
    pub protected_by_min: usize,
}

#[derive(Debug, serde::Serialize)]
pub struct BackupResult {
    pub key: String,
    pub size_bytes: u64,
    pub prune: PruneResult,
}

// ── R2 backup service ─────────────────────────────────────────────────────────

pub struct R2BackupService {
    account_id: String,
    bucket: String,
    access_key_id: String,
    secret_access_key: String,
    prefix: String,
}

impl R2BackupService {
    pub fn from_config(cfg: &R2Config) -> anyhow::Result<Self> {
        let access_key_id = cfg
            .resolved_access_key_id()
            .ok_or_else(|| anyhow::anyhow!(
                "R2 access_key_id not set; configure backup.r2.access_key_id \
                 or INNATE_R2_ACCESS_KEY_ID env var"
            ))?;
        let secret_access_key = cfg
            .resolved_secret_access_key()
            .ok_or_else(|| anyhow::anyhow!(
                "R2 secret_access_key not set; configure backup.r2.secret_access_key \
                 or INNATE_R2_SECRET_ACCESS_KEY env var"
            ))?;
        Ok(Self {
            account_id: cfg.account_id.clone(),
            bucket: cfg.bucket.clone(),
            access_key_id,
            secret_access_key,
            prefix: cfg.prefix.clone(),
        })
    }

    fn endpoint_base(&self) -> String {
        format!("https://{}.r2.cloudflarestorage.com", self.account_id)
    }

    fn host(&self) -> String {
        format!("{}.r2.cloudflarestorage.com", self.account_id)
    }

    // ── AWS Sig V4 ────────────────────────────────────────────────────────────

    fn sha256_hex(data: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(data);
        hex_bytes(&h.finalize())
    }

    fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
        let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key size");
        mac.update(data);
        mac.finalize().into_bytes().to_vec()
    }

    /// Returns `(Authorization header value, x-amz-content-sha256 value)`.
    fn sign(
        &self,
        method: &str,
        path: &str,  // URI path, e.g. "/bucket/key" (already URI-encoded)
        query: &str, // canonical query string (keys=values sorted, values URI-encoded)
        body: &[u8],
        datetime: &str, // "20240101T120000Z"
    ) -> (String, String) {
        let date = &datetime[..8];
        let payload_hash = Self::sha256_hex(body);
        let host = self.host();

        // Canonical headers — must be sorted by lowercase name.
        let mut hdrs: Vec<(String, String)> = vec![
            ("host".into(), host.clone()),
            ("x-amz-content-sha256".into(), payload_hash.clone()),
            ("x-amz-date".into(), datetime.into()),
        ];
        hdrs.sort_by(|a, b| a.0.cmp(&b.0));

        let canonical_headers: String = hdrs.iter().map(|(k, v)| format!("{}:{}\n", k, v)).collect();
        let signed_headers: String = hdrs.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");

        let canonical_request = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            method, path, query, canonical_headers, signed_headers, payload_hash
        );

        let credential_scope = format!("{}/auto/s3/aws4_request", date);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{}\n{}",
            datetime,
            credential_scope,
            Self::sha256_hex(canonical_request.as_bytes())
        );

        let k_date = Self::hmac_sha256(
            format!("AWS4{}", self.secret_access_key).as_bytes(),
            date.as_bytes(),
        );
        let k_region = Self::hmac_sha256(&k_date, b"auto");
        let k_service = Self::hmac_sha256(&k_region, b"s3");
        let k_signing = Self::hmac_sha256(&k_service, b"aws4_request");
        let signature = hex_bytes(&Self::hmac_sha256(&k_signing, string_to_sign.as_bytes()));

        let auth = format!(
            "AWS4-HMAC-SHA256 Credential={}/{},SignedHeaders={},Signature={}",
            self.access_key_id, credential_scope, signed_headers, signature
        );
        (auth, payload_hash)
    }

    fn now_datetime() -> String {
        chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
    }

    // ── HTTP operations ───────────────────────────────────────────────────────

    fn put_object(&self, key: &str, body: &[u8]) -> anyhow::Result<()> {
        let path = format!("/{}/{}", self.bucket, uri_encode_path(key));
        let datetime = Self::now_datetime();
        let (auth, payload_hash) = self.sign("PUT", &path, "", body, &datetime);
        let url = format!("{}{}", self.endpoint_base(), path);
        ureq::put(&url)
            .set("x-amz-date", &datetime)
            .set("x-amz-content-sha256", &payload_hash)
            .set("Authorization", &auth)
            .send_bytes(body)
            .map_err(|e| anyhow::anyhow!("R2 PUT {key}: {e}"))?;
        Ok(())
    }

    fn delete_object(&self, key: &str) -> anyhow::Result<()> {
        let path = format!("/{}/{}", self.bucket, uri_encode_path(key));
        let datetime = Self::now_datetime();
        let (auth, payload_hash) = self.sign("DELETE", &path, "", &[], &datetime);
        let url = format!("{}{}", self.endpoint_base(), path);
        match ureq::delete(&url)
            .set("x-amz-date", &datetime)
            .set("x-amz-content-sha256", &payload_hash)
            .set("Authorization", &auth)
            .call()
        {
            Ok(_) => Ok(()),
            Err(ureq::Error::Status(404, _)) => Ok(()), // already gone
            Err(e) => Err(anyhow::anyhow!("R2 DELETE {key}: {e}")),
        }
    }

    fn list_objects(&self) -> anyhow::Result<Vec<BackupInfo>> {
        // Build canonical query: list-type=2 (and prefix if set).
        // Values must be URI-encoded in the canonical query string.
        let mut query_params: Vec<(&str, String)> = vec![("list-type", "2".into())];
        if !self.prefix.is_empty() {
            query_params.push(("prefix", uri_encode_value(&self.prefix)));
        }
        query_params.sort_by_key(|(k, _)| *k);
        let canonical_query: String = query_params
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect::<Vec<_>>()
            .join("&");

        let path = format!("/{}", self.bucket);
        let datetime = Self::now_datetime();
        let (auth, payload_hash) = self.sign("GET", &path, &canonical_query, &[], &datetime);
        let url = format!("{}{}?{}", self.endpoint_base(), path, canonical_query);

        let body = ureq::get(&url)
            .set("x-amz-date", &datetime)
            .set("x-amz-content-sha256", &payload_hash)
            .set("Authorization", &auth)
            .call()
            .map_err(|e| anyhow::anyhow!("R2 LIST: {e}"))?
            .into_string()
            .map_err(|e| anyhow::anyhow!("R2 LIST read: {e}"))?;

        Ok(parse_list_xml(&body))
    }

    // ── Public API ────────────────────────────────────────────────────────────

    /// Create a consistent SQLite copy via VACUUM INTO, upload to R2, then prune old backups.
    pub fn backup_now(
        &self,
        db_path: &Path,
        retention_days: u64,
        min_backups: usize,
    ) -> anyhow::Result<BackupResult> {
        // Temporary file for the VACUUM copy.
        let tmp_dir = dirs_next::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".innate")
            .join("tmp");
        std::fs::create_dir_all(&tmp_dir)?;
        let ts_str = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
        let tmp_path = tmp_dir.join(format!("innate-backup-{ts_str}.db"));

        let vacuum_result = (|| -> anyhow::Result<Vec<u8>> {
            let conn = rusqlite::Connection::open(db_path)
                .map_err(|e| anyhow::anyhow!("Cannot open DB for backup: {e}"))?;
            conn.execute(
                "VACUUM INTO ?1",
                rusqlite::params![tmp_path.to_string_lossy().as_ref()],
            )
            .map_err(|e| anyhow::anyhow!("VACUUM INTO failed: {e}"))?;
            let bytes = std::fs::read(&tmp_path)
                .map_err(|e| anyhow::anyhow!("Cannot read backup temp file: {e}"))?;
            Ok(bytes)
        })();
        let _ = std::fs::remove_file(&tmp_path); // clean up regardless
        let body = vacuum_result?;

        let size_bytes = body.len() as u64;
        let key = format!("{}innate-backup-{ts_str}.db", self.prefix);
        self.put_object(&key, &body)?;

        let now = crate::utils::utc_now_iso();
        let _ = save_state(&BackupState {
            last_backup_at: Some(now),
            last_backup_key: Some(key.clone()),
        });

        let prune = self.prune_old_backups(retention_days, min_backups)?;

        Ok(BackupResult { key, size_bytes, prune })
    }

    /// List all backup objects in R2.
    pub fn list_backups(&self) -> anyhow::Result<Vec<BackupInfo>> {
        let mut backups = self.list_objects()?;
        backups.sort_by(|a, b| a.last_modified.cmp(&b.last_modified));
        Ok(backups)
    }

    /// Delete backups older than `retention_days`, but always keep at least `min_backups`.
    pub fn prune_old_backups(
        &self,
        retention_days: u64,
        min_backups: usize,
    ) -> anyhow::Result<PruneResult> {
        let mut backups = self.list_objects()?;
        // Oldest first.
        backups.sort_by(|a, b| a.last_modified.cmp(&b.last_modified));

        let cutoff = (chrono::Utc::now() - chrono::Duration::days(retention_days as i64))
            .format("%Y-%m-%dT%H:%M:%S")
            .to_string();

        let total = backups.len();
        let old_indices: Vec<usize> = backups
            .iter()
            .enumerate()
            .filter(|(_, b)| b.last_modified < cutoff)
            .map(|(i, _)| i)
            .collect();

        // We must keep at least min_backups total.
        let max_deletable = total.saturating_sub(min_backups);
        let deletable = old_indices.len().min(max_deletable);
        let protected = old_indices.len().saturating_sub(deletable);

        let to_delete: Vec<String> = old_indices
            .into_iter()
            .take(deletable)
            .map(|i| backups[i].key.clone())
            .collect();

        for key in &to_delete {
            self.delete_object(key)?;
        }

        Ok(PruneResult {
            kept: total - to_delete.len(),
            deleted: to_delete,
            protected_by_min: protected,
        })
    }

    // ── State helpers (no network) ────────────────────────────────────────────

    /// Returns true if a backup is due based on local state (no network call).
    pub fn needs_backup(interval_hours: u64) -> bool {
        let state = load_state();
        let Some(last_at) = state.last_backup_at else {
            return true;
        };
        // utc_now_iso() format: "2024-01-01T12:00:00.000Z"
        let Ok(last_dt) = chrono::DateTime::parse_from_rfc3339(&last_at) else {
            return true;
        };
        let elapsed = chrono::Utc::now().signed_duration_since(last_dt.with_timezone(&chrono::Utc));
        elapsed.num_hours() >= interval_hours as i64
    }

    pub fn last_backup_state() -> BackupState {
        load_state()
    }
}

// ── Auto-backup entry point (called by daemon and MCP/CLI) ───────────────────

/// Run a backup if the configured interval has elapsed since the last backup.
/// Returns true if a backup was performed.
pub fn maybe_auto_backup(db_path: &Path, cfg: &BackupConfig) -> anyhow::Result<bool> {
    if !cfg.enable {
        return Ok(false);
    }
    let r2_cfg = match &cfg.r2 {
        Some(c) => c,
        None => return Ok(false),
    };
    if !R2BackupService::needs_backup(cfg.auto_backup_interval_hours) {
        return Ok(false);
    }
    let svc = R2BackupService::from_config(r2_cfg)?;
    svc.backup_now(db_path, cfg.retention_days, cfg.min_backups)?;
    Ok(true)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// URI-encode a key path segment, preserving forward slashes.
fn uri_encode_path(s: &str) -> String {
    let mut out = String::new();
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(*b as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// URI-encode a query parameter value (encodes slashes too).
fn uri_encode_value(s: &str) -> String {
    let mut out = String::new();
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char);
            }
            other => {
                out.push_str(&format!("%{other:02X}"));
            }
        }
    }
    out
}

/// Parse S3 XML list-objects-v2 response into BackupInfo list.
fn parse_list_xml(body: &str) -> Vec<BackupInfo> {
    let mut results = Vec::new();
    for block in body.split("<Contents>").skip(1) {
        let key = xml_text(block, "Key");
        let lm = xml_text(block, "LastModified");
        let size = xml_text(block, "Size")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if let (Some(key), Some(last_modified)) = (key, lm) {
            results.push(BackupInfo { key, last_modified, size_bytes: size });
        }
    }
    results
}

fn xml_text(haystack: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = haystack.find(&open)? + open.len();
    let end = haystack[start..].find(&close)? + start;
    Some(haystack[start..end].to_owned())
}
