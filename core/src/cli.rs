//! CLI commands — thin wrapper over KnowledgeBase.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use serde_json::json;

fn default_db() -> PathBuf {
    dirs_next::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".innate")
        .join("personal.db")
}

#[derive(Parser)]
#[command(name = "innate", version, about = "Self-growing knowledge layer")]
pub struct Cli {
    #[arg(long, global = true, env = "INNATE_DB")]
    pub db: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Search the knowledge base
    Recall {
        query: String,
        #[arg(long, default_value = "6000")]
        budget: usize,
        #[arg(long)]
        top: Option<usize>,
        #[arg(long, default_value = "text")]
        format: String,
        #[arg(long)]
        include_sparks: bool,
        /// Dependency expansion: false (default) | direct | closure
        #[arg(long, default_value = "false")]
        expand_deps: String,
        /// Allow Refiner to trim blocks that don't fit the budget
        #[arg(long)]
        allow_trim: bool,
        /// Refine mode written to usage_trace: off (default) | trim | adapt
        #[arg(long, default_value = "off")]
        refine_mode: String,
        /// Event source written to usage_trace (mcp | sdk | cli | hook | daemon | augmented)
        #[arg(long, default_value = "cli")]
        source: String,
    },
    /// Close a trace with outcome
    Record {
        trace_id: String,
        #[arg(long)]
        query: Option<String>,
        #[arg(long)]
        outcome: Option<String>,
        /// Comma-separated chunk ids. An explicit empty value means "known none".
        #[arg(long)]
        used: Option<String>,
        #[arg(long, default_value = "explicit")]
        used_attribution: String,
        /// Treat --used as partial attribution; omitted selected chunks are not penalized.
        #[arg(long)]
        used_partial: bool,
        #[arg(long)]
        output: Option<String>,
        #[arg(long)]
        output_summary: Option<String>,
        #[arg(long)]
        nomination: Option<String>,
        #[arg(long, default_value = "cli")]
        source: String,
        /// Explicit feedback: up or down (applied to --used chunks if provided)
        #[arg(long)]
        feedback: Option<String>,
        #[arg(long, default_value = "user")]
        feedback_kind: String,
        #[arg(long)]
        feedback_actor: Option<String>,
        #[arg(long)]
        feedback_reason: Option<String>,
        #[arg(long)]
        task_state: Option<String>,
        #[arg(long, default_value = "0")]
        priority: i64,
    },
    /// Add a knowledge chunk
    Add {
        content: String,
        #[arg(long, default_value = "note")]
        kind: String,
        #[arg(long)]
        trigger: Option<String>,
        #[arg(long)]
        anti_trigger: Option<String>,
        #[arg(long, default_value = "chat")]
        source: String,
        #[arg(long)]
        skill_name: Option<String>,
    },
    /// Capture a spark (idea)
    Spark {
        content: String,
        #[arg(long)]
        trigger: Option<String>,
    },
    /// Distil logs + curate
    Evolve {
        #[arg(long, default_value = "manual")]
        trigger: String,
        /// Rebuild embeddings for chunks with embed_version=0 or < meta.embed_version
        #[arg(long)]
        rebuild_embeddings: bool,
    },
    /// Health check — no arg = library summary; chunk_id or trace_id = detail view
    Inspect { id: Option<String> },
    /// Approve a pending chunk
    Approve { chunk_id: String },
    /// Archive a chunk
    Archive {
        chunk_id: String,
        #[arg(long, default_value = "stale")]
        reason: String,
    },
    /// Invalidate a chunk
    Invalidate {
        chunk_id: String,
        #[arg(long, default_value = "")]
        reason: String,
    },
    /// Restore an archived chunk
    Restore { chunk_id: String },
    /// Mature a spark
    MatureSpark { spark_id: String, to: String },
    /// Promote a spark to knowledge
    PromoteSpark {
        spark_id: String,
        #[arg(long, default_value = "note")]
        to: String,
    },
    /// Drop a spark
    DropSpark {
        spark_id: String,
        #[arg(long, default_value = "")]
        reason: String,
    },
    /// Backup the database to Cloudflare R2
    Backup {
        #[command(subcommand)]
        action: BackupCommands,
    },
    /// Interactive setup wizard — configure agents to use Innate MCP server
    Install,
    /// Remove Innate from all configured agents and PATH
    Uninstall {
        /// Skip confirmation prompts
        #[arg(long, short = 'y')]
        yes: bool,
        /// Also delete knowledge data (~/.innate/). Cannot be undone.
        #[arg(long)]
        purge_data: bool,
    },
    /// Upgrade database schema to current version
    Migrate,
    /// Upgrade the innate binary to the latest (or specified) release
    Upgrade {
        /// Install this specific version, e.g. 0.3.0 or v0.3.0 (default: latest)
        #[arg(long, value_name = "VERSION")]
        version: Option<String>,
        /// Only report whether an upgrade is available; do not install
        #[arg(long)]
        check: bool,
    },
    /// Daemon control (Linux only)
    Daemon {
        #[command(subcommand)]
        action: DaemonCommands,
    },
    /// Start MCP stdio server
    Mcp,
    /// Agent hook handlers (called by agent hooks; reads payload from stdin)
    Hook {
        #[command(subcommand)]
        action: HookCommands,
    },
}

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

#[derive(Subcommand)]
pub enum HookCommands {
    /// Process a Claude Code Stop hook payload from stdin and record session events
    Stop,
}

#[derive(Subcommand)]
pub enum DaemonCommands {
    /// Start the background log-watcher daemon
    Start {
        #[arg(long = "watch", value_name = "LOG_DIR")]
        watch: Vec<std::path::PathBuf>,
        #[arg(long, value_name = "PATH")]
        pid_file: Option<std::path::PathBuf>,
        #[arg(long, value_name = "PATH")]
        state_db: Option<std::path::PathBuf>,
        #[arg(long, value_name = "PATH")]
        log_file: Option<std::path::PathBuf>,
    },
    /// Stop a running daemon
    Stop {
        #[arg(long, value_name = "PATH")]
        pid_file: Option<std::path::PathBuf>,
    },
    /// Show daemon status
    Status {
        #[arg(long, value_name = "PATH")]
        state_db: Option<std::path::PathBuf>,
        #[arg(long, value_name = "PATH")]
        pid_file: Option<std::path::PathBuf>,
    },
}

pub fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let db_path = cli.db.unwrap_or_else(default_db);

    if let Commands::Mcp = &cli.command {
        return crate::mcp::run_server(db_path);
    }

    if let Commands::Install = &cli.command {
        return crate::install::run_install();
    }

    if let Commands::Uninstall { yes, purge_data } = &cli.command {
        return crate::install::run_uninstall(*yes, *purge_data);
    }

    if let Commands::Migrate = &cli.command {
        let applied = crate::migrate::run_migrations(&db_path)?;
        if applied.is_empty() {
            println!("already at 4.14 — nothing to do");
        } else {
            for step in &applied {
                println!("  applied: {step}");
            }
            println!("migration complete");
        }
        return Ok(());
    }

    if let Commands::Daemon { action } = &cli.command {
        return run_daemon(action, &db_path);
    }

    if let Commands::Backup { action } = &cli.command {
        return run_backup(action, &db_path);
    }

    if let Commands::Upgrade { version, check } = &cli.command {
        return crate::upgrade::run_upgrade(version.as_deref(), &db_path, *check);
    }

    if let Commands::Hook { action } = &cli.command {
        return match action {
            HookCommands::Stop => run_hook_stop(),
        };
    }

    let kb = crate::open_kb(&db_path)?;

    match cli.command {
        Commands::Recall {
            query,
            budget,
            top,
            format,
            include_sparks,
            expand_deps,
            allow_trim,
            refine_mode,
            source,
        } => {
            let result = kb.recall(
                &query,
                budget,
                true,
                include_sparks,
                top,
                &source,
                &expand_deps,
                allow_trim,
                &refine_mode,
            )?;
            match format.as_str() {
                "json" => println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "trace_id": result.trace_id,
                        "knowledge": result.knowledge,
                        "sparks": result.sparks,
                        "empty": result.empty,
                    }))?
                ),
                "prompt" => {
                    for chunk in &result.knowledge {
                        let content = chunk.get("content").and_then(|v| v.as_str()).unwrap_or("");
                        println!("{content}\n---");
                    }
                    // metadata at end (§九 CLI contract)
                    println!("<!-- innate_trace_id: {} -->", result.trace_id);
                    println!(
                        "<!-- innate_selected: {} -->",
                        result
                            .knowledge
                            .iter()
                            .filter_map(|c| c.get("id").and_then(|v| v.as_str()))
                            .collect::<Vec<_>>()
                            .join(",")
                    );
                }
                _ => {
                    for chunk in &result.knowledge {
                        let id = chunk.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                        let content = chunk.get("content").and_then(|v| v.as_str()).unwrap_or("");
                        let conf = chunk
                            .get("confidence")
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.5);
                        println!("[{id}] (conf={conf:.2})\n{content}\n");
                    }
                    if result.empty {
                        println!("(no results)");
                    }
                }
            }
        }
        Commands::Record {
            trace_id,
            query,
            outcome,
            used,
            used_attribution,
            used_partial,
            output,
            output_summary,
            nomination,
            source,
            feedback,
            feedback_kind,
            feedback_actor,
            feedback_reason,
            task_state,
            priority,
        } => {
            let used_ids = used.as_deref().map(|raw| {
                raw.split(',')
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            });
            let used_ref = used_ids.as_deref();
            // Per §二·五B: trace-level "up" applies only to explicitly used chunks.
            let (fb_up, fb_down): (Option<Vec<String>>, Option<Vec<String>>) =
                match feedback.as_deref() {
                    Some("up") if used_ids.as_ref().is_some_and(|ids| !ids.is_empty()) => {
                        (used_ids.clone(), None)
                    }
                    Some("down") if used_ids.as_ref().is_some_and(|ids| !ids.is_empty()) => {
                        (None, used_ids.clone())
                    }
                    Some("up") => (None, None), // no used chunks — ignore per design
                    Some("down") => (None, None),
                    _ => (None, None),
                };
            let fb_up_ref = fb_up.as_deref();
            let fb_down_ref = fb_down.as_deref();
            kb.record_detailed(
                &trace_id,
                query.as_deref(),
                output.as_deref(),
                output_summary.as_deref(),
                outcome.as_deref(),
                used_ref,
                &used_attribution,
                !used_partial,
                fb_up_ref,
                fb_down_ref,
                &feedback_kind,
                feedback_actor.as_deref(),
                feedback_reason.as_deref(),
                nomination.as_deref(),
                priority,
                task_state.as_deref(),
                &source,
            )?;
            println!("recorded");
        }
        Commands::Add {
            content,
            kind,
            trigger,
            anti_trigger,
            source,
            skill_name,
        } => {
            // If kind=skill and content is a readable file path, load its content.
            let content = if kind == "skill" {
                let p = std::path::Path::new(&content);
                if p.exists() && p.is_file() {
                    std::fs::read_to_string(p).map_err(|e| {
                        anyhow::anyhow!("Failed to read skill file {}: {e}", p.display())
                    })?
                } else {
                    content
                }
            } else {
                content
            };
            let id = kb.add(
                &content,
                &kind,
                trigger.as_deref(),
                anti_trigger.as_deref(),
                &source,
                skill_name.as_deref(),
            )?;
            println!("{id}");
        }
        Commands::Spark { content, trigger } => {
            let id = kb.spark(&content, trigger.as_deref(), None)?;
            println!("{id}");
        }
        Commands::Evolve {
            trigger,
            rebuild_embeddings,
        } => {
            if rebuild_embeddings {
                let rebuilt = kb.rebuild_embeddings()?;
                let report = kb.evolve(&trigger)?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "rebuilt_embeddings": rebuilt,
                        "evolve": report
                    }))?
                );
            } else {
                let report = kb.evolve(&trigger)?;
                println!("{}", serde_json::to_string_pretty(&report)?);
            }
        }
        Commands::Inspect { id } => match id.as_deref() {
            None => {
                let info = kb.inspect()?;
                println!("{}", serde_json::to_string_pretty(&info)?);
            }
            Some(id) => {
                let detail = kb.inspect_id(id)?;
                println!("{}", serde_json::to_string_pretty(&detail)?);
            }
        },
        Commands::Approve { chunk_id } => {
            kb.approve(&chunk_id)?;
            println!("approved");
        }
        Commands::Archive { chunk_id, reason } => {
            kb.archive(&chunk_id, &reason)?;
            println!("archived");
        }
        Commands::Invalidate { chunk_id, reason } => {
            kb.invalidate(&chunk_id, &reason)?;
            println!("invalidated");
        }
        Commands::Restore { chunk_id } => {
            kb.restore(&chunk_id)?;
            println!("restored");
        }
        Commands::MatureSpark { spark_id, to } => {
            kb.mature_spark(&spark_id, &to)?;
            println!("matured");
        }
        Commands::PromoteSpark { spark_id, to } => {
            let id = kb.promote_spark(&spark_id, &to)?;
            println!("{id}");
        }
        Commands::DropSpark { spark_id, reason } => {
            kb.drop_spark(&spark_id, &reason)?;
            println!("dropped");
        }
        Commands::Mcp
        | Commands::Install
        | Commands::Uninstall { .. }
        | Commands::Migrate
        | Commands::Upgrade { .. }
        | Commands::Daemon { .. }
        | Commands::Backup { .. }
        | Commands::Hook { .. } => unreachable!(),
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Daemon implementation
// ---------------------------------------------------------------------------

fn default_pid_file() -> std::path::PathBuf {
    dirs_next::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".innate")
        .join("daemon.pid")
}

fn default_state_db() -> std::path::PathBuf {
    dirs_next::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".innate")
        .join("daemon_state.sqlite")
}

fn default_log_file() -> std::path::PathBuf {
    dirs_next::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".innate")
        .join("daemon.log")
}

fn run_daemon(action: &DaemonCommands, db_path: &std::path::Path) -> anyhow::Result<()> {
    match action {
        DaemonCommands::Start {
            watch,
            pid_file,
            state_db,
            log_file,
        } => {
            // Merge CLI --watch dirs with settings watch_dirs when CLI provides none.
            let effective_watch: Vec<std::path::PathBuf> = if !watch.is_empty() {
                watch.clone()
            } else {
                let s = crate::settings::load();
                crate::settings::resolved_watch_dirs(&s)
                    .into_iter()
                    .map(std::path::PathBuf::from)
                    .collect()
            };
            crate::daemon::start(
                &effective_watch,
                db_path,
                pid_file.as_deref().unwrap_or(&default_pid_file()),
                state_db.as_deref().unwrap_or(&default_state_db()),
                log_file.as_deref().unwrap_or(&default_log_file()),
            )
        }
        DaemonCommands::Stop { pid_file } => {
            crate::daemon::stop(pid_file.as_deref().unwrap_or(&default_pid_file()))
        }
        DaemonCommands::Status { state_db, pid_file } => crate::daemon::status(
            state_db.as_deref().unwrap_or(&default_state_db()),
            pid_file.as_deref().unwrap_or(&default_pid_file()),
        ),
    }
}

// ---------------------------------------------------------------------------
// Backup implementation
// ---------------------------------------------------------------------------

fn run_backup(action: &BackupCommands, db_path: &std::path::Path) -> anyhow::Result<()> {
    use crate::backup::R2BackupService;

    let settings = crate::settings::load();
    let cfg = settings.backup.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "No backup config found in ~/.innate/settings.json.\n\
             Add a \"backup\" section with \"enable\": true and \"r2\" credentials to enable R2 backup."
        )
    })?;

    // status works regardless of enable flag
    if let BackupCommands::Status = action {
        use crate::backup::R2BackupService;
        let state = R2BackupService::last_backup_state();
        println!("R2 backup enabled : {}", cfg.enable);
        println!("R2 bucket         : {}", cfg.r2.as_ref().map(|r| r.bucket.as_str()).unwrap_or("-"));
        println!("Last backup       : {}", state.last_backup_at.as_deref().unwrap_or("never"));
        println!("Last backup key   : {}", state.last_backup_key.as_deref().unwrap_or("-"));
        let due = R2BackupService::needs_backup(cfg.auto_backup_interval_hours);
        println!("Backup due        : {}", if cfg.enable && due { "yes" } else if !cfg.enable { "disabled" } else { "no" });
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
    let r2_cfg = cfg.r2.as_ref().ok_or_else(|| {
        anyhow::anyhow!("backup.r2 not configured in ~/.innate/settings.json")
    })?;

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

// ---------------------------------------------------------------------------
// Hook implementation
// ---------------------------------------------------------------------------

fn extract_content_text(content: Option<&serde_json::Value>) -> String {
    match content {
        None => String::new(),
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

fn run_hook_stop() -> anyhow::Result<()> {
    use std::io::{Read, Write};

    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;

    let data: serde_json::Value =
        serde_json::from_str(&input).unwrap_or(serde_json::Value::Null);

    let empty = vec![];
    let transcript = data
        .get("transcript")
        .or_else(|| data.get("messages"))
        .and_then(|v| v.as_array())
        .unwrap_or(&empty);

    let mut query = String::new();
    let mut summary = String::new();

    for m in transcript.iter().rev() {
        let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("");
        if query.is_empty() && role == "user" {
            query = extract_content_text(m.get("content"))
                .chars()
                .take(200)
                .collect();
        }
        if summary.is_empty() && role == "assistant" {
            summary = extract_content_text(m.get("content"))
                .chars()
                .take(400)
                .collect();
        }
        if !query.is_empty() && !summary.is_empty() {
            break;
        }
    }

    let mut events: Vec<serde_json::Value> = Vec::new();
    if !query.is_empty() {
        events.push(json!({"event_type": "session_start", "query": query.trim()}));
    }
    if !summary.is_empty() {
        events.push(json!({"event_type": "tool_success", "output_summary": summary.trim(), "outcome": "ok"}));
    }
    events.push(json!({"event_type": "session_end"}));

    let log_path = dirs_next::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".innate")
        .join("sessions")
        .join("session.log");

    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;

    for event in &events {
        writeln!(file, "{}", serde_json::to_string(event)?)?;
    }

    Ok(())
}
