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
    },
    /// Health check
    Inspect,
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
    /// Start MCP stdio server
    Mcp,
}

pub fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let db_path = cli.db.unwrap_or_else(default_db);

    if let Commands::Mcp = &cli.command {
        return crate::mcp::run_server(db_path);
    }

    let kb = KnowledgeBase::open(&db_path)?;

    match cli.command {
        Commands::Recall { query, budget, top, format, include_sparks } => {
            let result = kb.recall(&query, budget, true, include_sparks, top, "cli")?;
            match format.as_str() {
                "json" => println!("{}", serde_json::to_string_pretty(&json!({
                    "trace_id": result.trace_id,
                    "knowledge": result.knowledge,
                    "sparks": result.sparks,
                    "empty": result.empty,
                }))?),
                "prompt" => {
                    println!("<!-- innate_trace_id: {} -->", result.trace_id);
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
        Commands::Record { trace_id, outcome, used, output_summary, nomination, source } => {
            let used_ref: Option<&[String]> = if used.is_empty() { None } else { Some(&used) };
            kb.record(
                &trace_id, None, None,
                output_summary.as_deref(),
                outcome.as_deref(),
                used_ref, None, None,
                nomination.as_deref(), 0, &source,
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
        Commands::Evolve { trigger } => {
            let report = kb.evolve(&trigger)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Commands::Inspect => {
            let info = kb.inspect()?;
            println!("{}", serde_json::to_string_pretty(&info)?);
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
        Commands::Mcp => unreachable!(),
    }
    Ok(())
}
