//! CLI commands — thin wrapper over KnowledgeBase.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use serde_json::json;

pub use crate::backup::BackupCommands;
pub use crate::daemon::DaemonCommands;
pub use crate::hook::HookCommands;
use crate::{AppraiseParams, RecallParams, RecordParams, Situation, APPRAISE_ADVISORY};

fn default_db() -> PathBuf {
    crate::paths::default_db_path()
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
        /// Relevance gate: drop candidates whose fused score is below this value.
        /// Keeps always-on hooks high-frequency without injecting noise.
        #[arg(long)]
        min_score: Option<f64>,
        /// Session trace: open a trace for later record-correlation but record no
        /// `selected`/`retrieved` events. For callers (e.g. the daemon) that do
        /// not inject the recalled knowledge into a model context.
        #[arg(long)]
        session: bool,
        /// Deep recall: rerank the shortlist with the configured LLM (offline,
        /// latency-tolerant). No-op without an LLM; never used by hooks.
        #[arg(long)]
        rerank: bool,
    },
    /// Critic: judge how much footing exists for a candidate in a situation.
    /// Returns {valence, strength, tier, flagged_points} — never an answer.
    Appraise {
        /// Explicit question / instruction (optional).
        #[arg(long, default_value = "")]
        query: String,
        /// Current or last error text.
        #[arg(long)]
        last_error: Option<String>,
        /// Recent actions, comma-separated.
        #[arg(long)]
        recent_actions: Option<String>,
        /// Task stage (e.g. merge, implement, review).
        #[arg(long)]
        stage: Option<String>,
        /// File type / path summary in scope.
        #[arg(long)]
        file_context: Option<String>,
        /// Candidate answer under judgement (folded into resonance, sanitized, never echoed).
        #[arg(long)]
        candidate: Option<String>,
        #[arg(long)]
        top: Option<usize>,
        #[arg(long)]
        min_strength: Option<f64>,
        #[arg(long, default_value = "cli")]
        source: String,
        #[arg(long, default_value = "json")]
        format: String,
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
        /// This trace came from an `appraise` whose caution was heeded — the
        /// action was avoided, so the outcome is counterfactual and must NOT
        /// count toward the critic's calibration (provenance=counterfactual_censored).
        #[arg(long)]
        verdict_heeded: bool,
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
        /// Declare a dependency on another chunk id (repeatable).
        #[arg(long = "depends-on")]
        depends_on: Vec<String>,
        /// Dependency kind for --depends-on: hard (fail-closed) or soft (bonus).
        #[arg(long, default_value = "hard")]
        dep_kind: String,
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
    /// Observability: write a state-KPI snapshot now (for inspect().trends week-over-week)
    Metrics {
        #[command(subcommand)]
        action: MetricsAction,
    },
    /// Reclaim disk space: checkpoint the WAL and VACUUM the database
    Vacuum,
    /// Repair pre-fix trace pollution: drop false daemon `selected` events,
    /// recompute `selected_count`, and retire orphaned `open` episodic logs.
    RepairTraces {
        /// Report what would change without writing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Measure recall quality on a labeled set using the configured embedding
    /// provider. Reads JSONL ({"query": "...", "relevant_ids": ["id", ...]}) and
    /// reports P@1 / Recall@k / MRR / nDCG@k. The honest way to know whether
    /// retrieval accuracy is actually a problem before tuning weights.
    RecallEval {
        /// Path to a JSONL labels file (one {query, relevant_ids} object per line).
        labels: PathBuf,
        /// Cutoff k for Recall@k / nDCG@k and the recall `top` (default 10).
        #[arg(long, default_value = "10")]
        k: usize,
        /// Append the run summary (metrics + params + ts) to ~/.innate/logs/eval_runs.jsonl
        /// so offline eval can be compared against online metrics over time.
        #[arg(long)]
        save: bool,
    },
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
    /// Start a local web UI to view and govern the knowledge base
    Web {
        /// Address to bind (localhost only by default; exposing beyond is unsafe)
        #[arg(long, default_value = "127.0.0.1")]
        bind: String,
        /// Port to listen on
        #[arg(long, default_value_t = 8788)]
        port: u16,
        /// Disable the governance auth token (NOT recommended; leaves writes unauthenticated)
        #[arg(long)]
        no_token: bool,
        /// Required to bind a non-loopback address. Exposes the knowledge base to
        /// the network; the auth token then gates reads as well as writes.
        #[arg(long)]
        allow_remote: bool,
    },
    /// Agent hook handlers (called by agent hooks; reads payload from stdin)
    Hook {
        #[command(subcommand)]
        action: HookCommands,
    },
}

#[derive(clap::Subcommand)]
pub enum MetricsAction {
    /// Write a state-KPI snapshot row now (debt ratio, pending age, success rates …).
    Snapshot,
}

pub fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    // Create the ~/.innate subdirectory layout and migrate any legacy flat files
    // before any path is resolved.
    crate::paths::ensure_layout();
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
            println!(
                "already at {} — nothing to do",
                crate::migrate::target_version()
            );
        } else {
            for step in &applied {
                println!("  applied: {step}");
            }
            println!("migration complete");
        }
        return Ok(());
    }

    if let Commands::Daemon { action } = &cli.command {
        return crate::daemon::run_command(action, &db_path);
    }

    if let Commands::Backup { action } = &cli.command {
        return crate::backup::run_command(action, &db_path);
    }

    if let Commands::Upgrade { version, check } = &cli.command {
        return crate::upgrade::run_upgrade(version.as_deref(), &db_path, *check);
    }

    if let Commands::Hook { action } = &cli.command {
        return crate::hook::run_command(action, &db_path);
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
            min_score,
            session,
            rerank,
        } => {
            let result = kb.recall(RecallParams {
                query: &query,
                budget,
                trace: true,
                include_sparks,
                top,
                source: &source,
                expand_deps: &expand_deps,
                allow_trim,
                refine_mode: &refine_mode,
                min_score,
                session_only: session,
                rerank,
            })?;
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
        Commands::Appraise {
            query,
            last_error,
            recent_actions,
            stage,
            file_context,
            candidate,
            top,
            min_strength,
            source,
            format,
        } => {
            let actions: Vec<String> = recent_actions
                .as_deref()
                .map(|raw| {
                    raw.split(',')
                        .map(str::trim)
                        .filter(|a| !a.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let situation = Situation {
                query: (!query.is_empty()).then_some(query.as_str()),
                last_error: last_error.as_deref(),
                recent_actions: &actions,
                stage: stage.as_deref(),
                file_context: file_context.as_deref(),
            };
            let verdict = kb.appraise(AppraiseParams {
                situation,
                candidate: candidate.as_deref(),
                min_strength,
                top,
                trace: true,
                source: &source,
            })?;
            match format.as_str() {
                "text" => {
                    println!("ℹ {APPRAISE_ADVISORY}");
                    if verdict.abstained {
                        println!(
                            "ABSTAIN reason={:?} strength={:.3} trace_id={}",
                            verdict.abstain_reason, verdict.strength, verdict.trace_id
                        );
                    } else {
                        println!(
                            "valence={:?} tier={:?} strength={:.3} confidence={:.3} dispersion={:.3} trace_id={}",
                            verdict.valence, verdict.tier, verdict.strength,
                            verdict.confidence, verdict.dispersion, verdict.trace_id
                        );
                    }
                    for fp in &verdict.flagged_points {
                        println!(
                            "  ⚠ [{}] {} (s={:.3})",
                            fp.chunk_id, fp.summary, fp.strength
                        );
                    }
                }
                _ => println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "advisory": APPRAISE_ADVISORY,
                        "valence": verdict.valence,
                        "strength": verdict.strength,
                        "tier": verdict.tier,
                        "confidence": verdict.confidence,
                        "dispersion": verdict.dispersion,
                        "abstained": verdict.abstained,
                        "abstain_reason": verdict.abstain_reason,
                        "flagged_points": verdict.flagged_points,
                        "contributors": verdict.contributors,
                        "trace_id": verdict.trace_id,
                    }))?
                ),
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
            verdict_heeded,
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
            kb.record(RecordParams {
                trace_id: &trace_id,
                query: query.as_deref(),
                output: output.as_deref(),
                output_summary: output_summary.as_deref(),
                outcome: outcome.as_deref(),
                used: used_ref,
                used_attribution: &used_attribution,
                used_complete: Some(!used_partial),
                feedback_up: fb_up_ref,
                feedback_down: fb_down_ref,
                feedback_kind: &feedback_kind,
                feedback_actor: feedback_actor.as_deref(),
                feedback_reason: feedback_reason.as_deref(),
                nomination: nomination.as_deref(),
                priority,
                task_state: task_state.as_deref(),
                source: &source,
                verdict_heeded,
            })?;
            println!("recorded");
        }
        Commands::Add {
            content,
            kind,
            trigger,
            anti_trigger,
            source,
            skill_name,
            depends_on,
            dep_kind,
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
            let deps: Vec<(String, String)> = depends_on
                .iter()
                .map(|d| (d.clone(), dep_kind.clone()))
                .collect();
            let id = kb.add_with_deps(
                &content,
                &kind,
                trigger.as_deref(),
                anti_trigger.as_deref(),
                &source,
                skill_name.as_deref(),
                &deps,
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
        Commands::Metrics { action } => match action {
            MetricsAction::Snapshot => {
                let kpis = kb.write_metric_snapshot()?;
                println!("{}", serde_json::to_string_pretty(&kpis)?);
            }
        },
        Commands::Vacuum => {
            let (before, after) = kb.storage.vacuum()?;
            let mb = |b: i64| b as f64 / 1_048_576.0;
            println!(
                "vacuumed: {:.2} MB → {:.2} MB (reclaimed {:.2} MB)",
                mb(before),
                mb(after),
                mb(before - after)
            );
        }
        Commands::RepairTraces { dry_run } => {
            let r = kb.repair_traces(dry_run)?;
            let tag = if dry_run {
                "[dry-run] would repair"
            } else {
                "repaired"
            };
            println!(
                "{tag}: deleted {} false daemon selection events, retired {} orphaned open logs, \
                 selected_count {} → {}",
                r.daemon_events_deleted, r.open_logs_retired, r.selected_before, r.selected_after
            );
        }
        Commands::RecallEval { labels, k, save } => {
            let text = std::fs::read_to_string(&labels)
                .map_err(|e| anyhow::anyhow!("read labels {}: {e}", labels.display()))?;
            let mut n = 0usize;
            let (mut sum_p1, mut sum_recall, mut sum_mrr, mut sum_ndcg) = (0.0, 0.0, 0.0, 0.0);
            let mut misses: Vec<serde_json::Value> = Vec::new();
            for (lineno, line) in text.lines().enumerate() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let row: serde_json::Value = serde_json::from_str(line)
                    .map_err(|e| anyhow::anyhow!("labels line {}: {e}", lineno + 1))?;
                let query = row.get("query").and_then(|v| v.as_str()).unwrap_or("");
                let relevant: std::collections::HashSet<String> = row
                    .get("relevant_ids")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                if query.is_empty() || relevant.is_empty() {
                    continue;
                }
                let result = kb.recall(RecallParams {
                    query,
                    budget: 100_000,
                    trace: false,
                    top: Some(k),
                    source: "cli",
                    ..Default::default()
                })?;
                let ranked: Vec<String> = result
                    .knowledge
                    .iter()
                    .filter_map(|c| c.get("id").and_then(|v| v.as_str()).map(str::to_string))
                    .collect();
                let (p1, recall_k, mrr, ndcg) = recall_metrics(&ranked, &relevant, k);
                sum_p1 += p1;
                sum_recall += recall_k;
                sum_mrr += mrr;
                sum_ndcg += ndcg;
                n += 1;
                // Per-query miss report (P4): surface queries where no relevant chunk
                // ranked, with the actual top-k, to debug *why* recall missed.
                if recall_k == 0.0 {
                    misses.push(json!({
                        "query": query,
                        "relevant_ids": relevant.iter().cloned().collect::<Vec<_>>(),
                        "got_top_k": ranked,
                    }));
                }
            }
            if n == 0 {
                return Err(anyhow::anyhow!(
                    "no usable labeled queries (need lines with non-empty query + relevant_ids)"
                ));
            }
            let nf = n as f64;
            let out = json!({
                "queries": n,
                "k": k,
                "p_at_1": (sum_p1 / nf * 1000.0).round() / 1000.0,
                "recall_at_k": (sum_recall / nf * 1000.0).round() / 1000.0,
                "mrr": (sum_mrr / nf * 1000.0).round() / 1000.0,
                "ndcg_at_k": (sum_ndcg / nf * 1000.0).round() / 1000.0,
                // Params snapshot — fused-score weights in effect, so an eval run is
                // self-describing and comparable against online metrics over time (P4).
                "params": kb.recall_weights(),
                "misses": misses,
            });
            if save {
                // Persist a compact run summary (no per-query misses) for trend comparison
                // against online metrics. Append-only JSONL; best-effort, non-fatal.
                let mut summary = out.clone();
                if let Some(o) = summary.as_object_mut() {
                    o.remove("misses");
                    o.insert("ts".to_string(), json!(crate::utils::utc_now_iso()));
                }
                let path = crate::paths::logs_dir().join("eval_runs.jsonl");
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                {
                    use std::io::Write;
                    let _ = writeln!(f, "{}", serde_json::to_string(&summary)?);
                    eprintln!("eval run summary appended to {}", path.display());
                }
            }
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        Commands::Web {
            bind,
            port,
            no_token,
            allow_remote,
        } => {
            let loopback = crate::web::is_loopback(&bind);
            if !loopback && !allow_remote {
                anyhow::bail!(
                    "refusing to bind non-loopback address {bind} without --allow-remote \
                     (this exposes the knowledge base to the network)"
                );
            }
            if !loopback && no_token {
                anyhow::bail!(
                    "--no-token cannot be combined with a non-loopback bind: a network-exposed \
                     server must keep the auth token to gate reads and writes"
                );
            }
            crate::web::serve(kb, &bind, port, !no_token)?;
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

/// Pure ranking metrics for a single query (part b — measurable recall quality).
/// `ranked` is the recalled chunk ids in rank order; `relevant` the labeled
/// ground-truth set. Returns `(p_at_1, recall_at_k, mrr, ndcg_at_k)`, each in
/// `[0,1]`. Kept IO-free so it is unit-testable without a database.
pub(crate) fn recall_metrics(
    ranked: &[String],
    relevant: &std::collections::HashSet<String>,
    k: usize,
) -> (f64, f64, f64, f64) {
    let topk = &ranked[..ranked.len().min(k)];
    let p_at_1 = topk
        .first()
        .map(|id| relevant.contains(id) as u8 as f64)
        .unwrap_or(0.0);
    let hits = topk.iter().filter(|id| relevant.contains(*id)).count();
    let recall_at_k = hits as f64 / relevant.len() as f64;
    // MRR over the full ranking: reciprocal rank of the first relevant hit.
    let mrr = ranked
        .iter()
        .position(|id| relevant.contains(id))
        .map(|pos| 1.0 / (pos as f64 + 1.0))
        .unwrap_or(0.0);
    // nDCG@k with binary relevance. IDCG = ideal placement of min(|rel|, k) hits.
    let dcg: f64 = topk
        .iter()
        .enumerate()
        .filter(|(_, id)| relevant.contains(*id))
        .map(|(i, _)| 1.0 / ((i as f64 + 2.0).log2()))
        .sum();
    let ideal_hits = relevant.len().min(k);
    let idcg: f64 = (0..ideal_hits)
        .map(|i| 1.0 / ((i as f64 + 2.0).log2()))
        .sum();
    let ndcg = if idcg > 0.0 { dcg / idcg } else { 0.0 };
    (p_at_1, recall_at_k, mrr, ndcg)
}

#[cfg(test)]
mod metric_tests {
    use super::recall_metrics;
    use std::collections::HashSet;

    fn rel(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }
    fn ranked(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn perfect_ranking_scores_one() {
        let (p1, r, mrr, ndcg) = recall_metrics(&ranked(&["a", "b", "x"]), &rel(&["a", "b"]), 5);
        assert!((p1 - 1.0).abs() < 1e-9);
        assert!((r - 1.0).abs() < 1e-9);
        assert!((mrr - 1.0).abs() < 1e-9);
        assert!((ndcg - 1.0).abs() < 1e-9);
    }

    #[test]
    fn missed_first_lowers_p1_and_mrr() {
        // Relevant item is at rank 2 → P@1=0, MRR=0.5, Recall@5=1.0.
        let (p1, r, mrr, _ndcg) = recall_metrics(&ranked(&["x", "a"]), &rel(&["a"]), 5);
        assert_eq!(p1, 0.0);
        assert!((mrr - 0.5).abs() < 1e-9);
        assert!((r - 1.0).abs() < 1e-9);
    }

    #[test]
    fn k_cutoff_limits_recall() {
        // Only the first id counts at k=1; the relevant one at rank 2 is excluded.
        let (_p1, r, _mrr, ndcg) = recall_metrics(&ranked(&["x", "a"]), &rel(&["a"]), 1);
        assert_eq!(r, 0.0);
        assert_eq!(ndcg, 0.0);
    }

    #[test]
    fn no_hits_is_all_zero() {
        let (p1, r, mrr, ndcg) = recall_metrics(&ranked(&["x", "y"]), &rel(&["a"]), 5);
        assert_eq!((p1, r, mrr, ndcg), (0.0, 0.0, 0.0, 0.0));
    }
}
