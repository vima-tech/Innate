#[derive(Subcommand)]
pub enum BackupCommands {
    /// Backup the database now (respects auto_backup_interval_hours check; use --force to skip)
    Run {
        /// Skip the 24-hour interval check and backup immediately
        #[arg(long)]
        force: bool,
    },
    /// Show last backup time and R2 config status
    Status,
    /// List all backups stored in R2
    List,
    /// Delete backups older than retention_days (keeps min_backups regardless)
    Prune,
}
pub(crate) fn run_command(action: &BackupCommands, db_path: &Path) -> anyhow::Result<()> {
    use super::R2BackupService;

    let settings = crate::settings::load()?;
    let cfg = settings.backup.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "No backup config found in ~/.innate/settings.json.\n\
             Add a \"backup\" section with \"enable\": true and \"r2\" credentials to enable R2 backup."
        )
    })?;

    // status works regardless of enable flag
    if let BackupCommands::Status = action {
        let state = R2BackupService::last_backup_state();
        println!("R2 backup enabled : {}", cfg.enable);
        println!(
            "R2 bucket         : {}",
            cfg.r2.as_ref().map(|r| r.bucket.as_str()).unwrap_or("-")
        );
        println!(
            "Last backup       : {}",
            state.last_backup_at.as_deref().unwrap_or("never")
        );
        println!(
            "Last backup key   : {}",
            state.last_backup_key.as_deref().unwrap_or("-")
        );
        let due = R2BackupService::needs_backup(cfg.auto_backup_interval_hours);
        println!(
            "Backup due        : {}",
            if cfg.enable && due {
                "yes"
            } else if !cfg.enable {
                "disabled"
            } else {
                "no"
            }
        );
        println!("Interval (h)      : {}", cfg.auto_backup_interval_hours);
        println!("Retention (days)  : {}", cfg.retention_days);
        println!("Min backups       : {}", cfg.min_backups);
        return Ok(());
    }

    if !cfg.enable {
        anyhow::bail!(
            "R2 backup is disabled (backup.enable = false).\n\
             Set \"enable\": true in the backup section of ~/.innate/settings.json to activate."
        );
    }
    let r2_cfg = cfg
        .r2
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("backup.r2 not configured in ~/.innate/settings.json"))?;

    match action {
        BackupCommands::Run { force } => {
            if !force && !R2BackupService::needs_backup(cfg.auto_backup_interval_hours) {
                let state = R2BackupService::last_backup_state();
                println!(
                    "backup not due yet (last: {}; interval: {}h). Use --force to override.",
                    state.last_backup_at.as_deref().unwrap_or("never"),
                    cfg.auto_backup_interval_hours
                );
                return Ok(());
            }
            println!("Starting backup to R2 bucket '{}'…", r2_cfg.bucket);
            let svc = R2BackupService::from_config(r2_cfg)?;
            let result = svc.backup_now(db_path, cfg.retention_days, cfg.min_backups)?;
            println!("Backed up: {} ({} bytes)", result.key, result.size_bytes);
            if !result.prune.deleted.is_empty() {
                println!("Pruned {} old backup(s):", result.prune.deleted.len());
                for k in &result.prune.deleted {
                    println!("  - {k}");
                }
            }
            if result.prune.protected_by_min > 0 {
                println!(
                    "  ({} old backup(s) kept to satisfy min_backups={})",
                    result.prune.protected_by_min, cfg.min_backups
                );
            }
            println!("Done. {} backup(s) remain in R2.", result.prune.kept);
        }
        BackupCommands::Status => unreachable!(), // handled above
        BackupCommands::List => {
            let svc = R2BackupService::from_config(r2_cfg)?;
            let backups = svc.list_backups()?;
            if backups.is_empty() {
                println!("No backups found in R2.");
            } else {
                println!("{} backup(s):", backups.len());
                for b in &backups {
                    println!("  {} | {} | {} bytes", b.last_modified, b.key, b.size_bytes);
                }
            }
        }
        BackupCommands::Prune => {
            let svc = R2BackupService::from_config(r2_cfg)?;
            let result = svc.prune_old_backups(cfg.retention_days, cfg.min_backups)?;
            if result.deleted.is_empty() {
                println!("Nothing to prune ({} backup(s) kept).", result.kept);
            } else {
                println!("Deleted {} backup(s):", result.deleted.len());
                for k in &result.deleted {
                    println!("  - {k}");
                }
                if result.protected_by_min > 0 {
                    println!(
                        "  ({} old backup(s) kept to satisfy min_backups={})",
                        result.protected_by_min, cfg.min_backups
                    );
                }
                println!("{} backup(s) remain.", result.kept);
            }
        }
    }
    Ok(())
}
use std::path::Path;

use clap::Subcommand;
