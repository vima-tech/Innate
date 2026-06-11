//! CLI commands — thin wrapper over KnowledgeBase.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use serde_json::json;

use crate::kb::KnowledgeBase;

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
    },
    /// Close a trace with outcome
    Record {
        trace_id: String,
        #[arg(long)]
        outcome: Option<String>,
        #[arg(long, value_delimiter = ',')]
        used: Vec<String>,
        #[arg(long)]
        output_summary: Option<String>,
        #[arg(long)]
        nomination: Option<String>,
        #[arg(long, default_value = "cli")]
        source: String,
        /// Explicit feedback: up or down (applied to --used chunks if provided)
        #[arg(long)]
        feedback: Option<String>,
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
    Inspect {
        id: Option<String>,
    },
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
    /// Interactive setup wizard — configure agents to use Innate MCP server
    Install,
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

    if let Commands::Migrate = &cli.command {
        let applied = crate::migrate::run_migrations(&db_path)?;
        if applied.is_empty() {
            println!("already at 4.5.1 — nothing to do");
        } else {
            for step in &applied { println!("  applied: {step}"); }
            println!("migration complete");
        }
        return Ok(());
    }

    if let Commands::Daemon { action } = &cli.command {
        return run_daemon(action, &db_path);
    }

    if let Commands::Upgrade { version, check } = &cli.command {
        return crate::upgrade::run_upgrade(version.as_deref(), &db_path, *check);
    }

    let kb = KnowledgeBase::open(&db_path)?;

    match cli.command {
        Commands::Recall { query, budget, top, format, include_sparks, expand_deps, allow_trim, refine_mode } => {
            let result = kb.recall(&query, budget, true, include_sparks, top, "cli", &expand_deps, allow_trim, &refine_mode)?;
            match format.as_str() {
                "json" => println!("{}", serde_json::to_string_pretty(&json!({
                    "trace_id": result.trace_id,
                    "knowledge": result.knowledge,
                    "sparks": result.sparks,
                    "empty": result.empty,
                }))?),
                "prompt" => {
                    println!("<!-- innate_trace_id: {} -->", result.trace_id);
                    println!("<!-- innate_selected: {} -->",
                        result.knowledge.iter()
                            .filter_map(|c| c.get("id").and_then(|v| v.as_str()))
                            .collect::<Vec<_>>().join(","));
                    for chunk in &result.knowledge {
                        let content = chunk.get("content").and_then(|v| v.as_str()).unwrap_or("");
                        println!("{content}\n---");
                    }
                }
                _ => {
                    println!("trace_id: {}", result.trace_id);
                    for chunk in &result.knowledge {
                        let id = chunk.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                        let content = chunk.get("content").and_then(|v| v.as_str()).unwrap_or("");
                        let conf = chunk.get("confidence").and_then(|v| v.as_f64()).unwrap_or(0.5);
                        println!("[{id}] (conf={conf:.2})\n{content}\n");
                    }
                    if result.empty { println!("(no results)"); }
                }
            }
        }
        Commands::Record { trace_id, outcome, used, output_summary, nomination, source, feedback, priority } => {
            let used_ref: Option<&[String]> = if used.is_empty() { None } else { Some(&used) };
            // Per §二·五B: trace-level "up" applies only to explicitly used chunks.
            let (fb_up, fb_down): (Option<Vec<String>>, Option<Vec<String>>) = match feedback.as_deref() {
                Some("up")   if !used.is_empty() => (Some(used.clone()), None),
                Some("down") if !used.is_empty() => (None, Some(used.clone())),
                Some("up")   => (None, None), // no used chunks — ignore per design
                Some("down") => (None, None),
                _ => (None, None),
            };
            let fb_up_ref   = fb_up.as_deref();
            let fb_down_ref = fb_down.as_deref();
            kb.record(
                &trace_id, None, None,
                output_summary.as_deref(),
                outcome.as_deref(),
                used_ref, fb_up_ref, fb_down_ref,
                nomination.as_deref(), priority, &source,
            )?;
            println!("recorded");
        }
        Commands::Add { content, kind, trigger, anti_trigger, source, skill_name } => {
            let id = kb.add(&content, &kind, trigger.as_deref(), anti_trigger.as_deref(), &source, skill_name.as_deref())?;
            println!("{id}");
        }
        Commands::Spark { content, trigger } => {
            let id = kb.spark(&content, trigger.as_deref(), None)?;
            println!("{id}");
        }
        Commands::Evolve { trigger, rebuild_embeddings } => {
            if rebuild_embeddings {
                let rebuilt = kb.rebuild_embeddings()?;
                println!("rebuilt {rebuilt} embeddings");
            } else {
                let report = kb.evolve(&trigger)?;
                println!("{}", serde_json::to_string_pretty(&report)?);
            }
        }
        Commands::Inspect { id } => {
            match id.as_deref() {
                None => {
                    let info = kb.inspect()?;
                    println!("{}", serde_json::to_string_pretty(&info)?);
                }
                Some(id) => {
                    let detail = kb.inspect_id(id)?;
                    println!("{}", serde_json::to_string_pretty(&detail)?);
                }
            }
        }
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
        Commands::Mcp | Commands::Install | Commands::Migrate | Commands::Upgrade { .. } | Commands::Daemon { .. } => unreachable!(),
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
        DaemonCommands::Start { watch, pid_file, state_db, log_file } => {
            crate::daemon::start(
                watch,
                db_path,
                pid_file.as_deref().unwrap_or(&default_pid_file()),
                state_db.as_deref().unwrap_or(&default_state_db()),
                log_file.as_deref().unwrap_or(&default_log_file()),
            )
        }
        DaemonCommands::Stop { pid_file } => {
            crate::daemon::stop(pid_file.as_deref().unwrap_or(&default_pid_file()))
        }
        DaemonCommands::Status { state_db } => {
            crate::daemon::status(state_db.as_deref().unwrap_or(&default_state_db()))
        }
    }
}
