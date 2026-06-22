//! MCP server — JSON-RPC 2.0 over stdio.
//!
//! Implements the Model Context Protocol (MCP) so any MCP-compatible host
//! (Claude Code, Claude Desktop, etc.) can call Innate without a separate SDK.
//!
//! Usage:  innate mcp   (reads from stdin, writes to stdout)

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::kb::{
    AppraiseParams, KnowledgeBase, RecallParams, RecordParams, Situation, APPRAISE_ADVISORY,
};

#[cfg(target_os = "linux")]
use libc;

// Tool names
const TOOLS: &[(&str, &str)] = &[
    ("innate_recall",         "Call FIRST at the start of any task — retrieve relevant knowledge from the knowledge base and get a trace_id for subsequent recording."),
    ("innate_record",         "Call LAST after completing any task — close the trace_id from recall with outcome ok/fail/unknown and optional feedback."),
    ("innate_appraise",       "Critic check — given a situation (and optional candidate answer), return how much footing exists: {valence, strength, tier, confidence, dispersion, abstained, abstain_reason, flagged_points}. May abstain (abstained=true) when there is no footing — abstaining is correct, not a failure. Never returns an answer. Use to gut-check before committing to a risky step; flagged_points = things to be careful about."),
    ("innate_add",            "Capture a confirmed insight as a knowledge chunk (always starts as pending for agent source)."),
    ("innate_spark",          "Save a quick idea / hypothesis for later incubation."),
    ("innate_inspect",        "Show knowledge base health: chunk counts, debt ratio, embed rebuild queue."),
    ("innate_evolve",         "Call at session end — distil episodic logs into pending chunks and run curate cycle (archive / decay / promote). Pass rebuild_embeddings=true to also rebuild the embedding index."),
    ("innate_approve",        "Approve a pending chunk, making it active."),
    ("innate_archive",        "Archive a knowledge chunk."),
    ("innate_invalidate",     "Invalidate a chunk and blacklist its content hash."),
    ("innate_restore",        "Restore an archived chunk to active."),
    ("innate_mature_spark",   "Advance a spark maturity: seed → sprouting → incubating."),
    ("innate_promote_spark",  "Promote a spark to a full knowledge chunk."),
    ("innate_drop_spark",     "Drop (abandon) a spark."),
    ("innate_backup",         "Backup or inspect R2 backups. action: run=backup now (honours interval unless force=true), status=show state, list=list backups in R2, prune=delete old backups."),
];

fn default_mcp_log() -> PathBuf {
    crate::paths::mcp_log_path()
}

fn mcp_log(log: &Mutex<Box<dyn io::Write + Send>>, msg: &str) {
    let ts = crate::utils::utc_now_iso();
    if let Ok(mut w) = log.lock() {
        let _ = writeln!(w, "{ts} {msg}");
    }
}

pub fn run_server(db_path: PathBuf) -> anyhow::Result<()> {
    crate::paths::ensure_layout();
    maybe_auto_start_daemon(&db_path);

    let log_path = default_mcp_log();
    if let Some(p) = log_path.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    let log: Mutex<Box<dyn io::Write + Send>> = Mutex::new(
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            Ok(f) => Box::new(f),
            Err(_) => Box::new(io::sink()),
        },
    );
    mcp_log(&log, &format!("[mcp] started db={}", db_path.display()));

    let kb = Mutex::new(crate::open_kb(&db_path)?);
    let stdin = io::stdin();
    let stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                mcp_log(&log, &format!("[mcp] parse error: {e}"));
                write_response(
                    &stdout,
                    json!({
                        "jsonrpc": "2.0",
                        "error": {"code": -32700, "message": format!("Parse error: {e}")},
                        "id": null
                    }),
                )?;
                continue;
            }
        };

        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or(json!({}));

        let response = match method {
            "initialize" => handle_initialize(&id),
            "tools/list" => handle_tools_list(&id),
            "tools/call" => handle_tool_call(&kb, &id, &params, &log),
            "notifications/initialized" | "ping" => continue,
            _ => json!({
                "jsonrpc": "2.0",
                "error": {"code": -32601, "message": format!("Method not found: {method}")},
                "id": id
            }),
        };

        write_response(&stdout, response)?;
    }
    Ok(())
}

fn write_response(stdout: &io::Stdout, v: Value) -> io::Result<()> {
    let mut out = stdout.lock();
    writeln!(out, "{}", serde_json::to_string(&v).unwrap_or_default())?;
    out.flush()
}

fn handle_initialize(id: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {
                "name": "innate",
                "version": env!("CARGO_PKG_VERSION")
            }
        }
    })
}

fn handle_tools_list(id: &Value) -> Value {
    let tools: Vec<Value> = TOOLS
        .iter()
        .map(|(name, desc)| {
            json!({
                "name": name,
                "description": desc,
                "inputSchema": tool_schema(name)
            })
        })
        .collect();
    json!({"jsonrpc": "2.0", "id": id, "result": {"tools": tools}})
}

fn handle_tool_call(
    kb: &Mutex<KnowledgeBase>,
    id: &Value,
    params: &Value,
    log: &Mutex<Box<dyn io::Write + Send>>,
) -> Value {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    let result = {
        let kb = kb.lock().unwrap_or_else(|e| e.into_inner());
        dispatch(&kb, name, &args)
    };

    match result {
        Ok(v) => {
            mcp_log(log, &format!("[mcp] tool={name} ok"));
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{"type": "text", "text": v.to_string()}],
                    "isError": false
                }
            })
        }
        Err(e) => {
            mcp_log(log, &format!("[mcp] tool={name} err={e}"));
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{"type": "text", "text": format!("error: {e}")}],
                    "isError": true
                }
            })
        }
    }
}

fn dispatch(kb: &KnowledgeBase, name: &str, args: &Value) -> crate::errors::Result<Value> {
    let s = |key: &str| {
        args.get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let so = |key: &str| args.get(key).and_then(Value::as_str).map(str::to_string);
    let b = |key: &str, d: bool| args.get(key).and_then(Value::as_bool).unwrap_or(d);
    let n = |key: &str, d: i64| args.get(key).and_then(Value::as_i64).unwrap_or(d);
    let arr = |key: &str| -> Vec<String> {
        args.get(key)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };

    match name {
        "innate_recall" => {
            let query = s("query");
            let budget = n("budget", 6000) as usize;
            let top = args.get("top").and_then(Value::as_u64).map(|v| v as usize);
            let include_sparks = b("include_sparks", false);
            let source = if s("source").is_empty() {
                "mcp".to_string()
            } else {
                s("source")
            };
            let expand_deps = if s("expand_deps").is_empty() {
                "false".to_string()
            } else {
                s("expand_deps")
            };
            let allow_trim = b("allow_trim", false);
            let refine_mode = if s("refine_mode").is_empty() {
                "off".to_string()
            } else {
                s("refine_mode")
            };
            let min_score = args.get("min_score").and_then(Value::as_f64);
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
                session_only: false,
                // MCP recall stays no-LLM by default; deep rerank is CLI-only ("deep recall").
                rerank: false,
            })?;
            Ok(json!({
                "trace_id": result.trace_id,
                "knowledge": result.knowledge,
                "sparks": result.sparks,
                "empty": result.empty,
            }))
        }
        "innate_appraise" => {
            let query = s("query");
            let last_error = so("last_error");
            let stage = so("stage");
            let file_context = so("file_context");
            let candidate = so("candidate");
            let recent_actions = arr("recent_actions");
            let top = args.get("top").and_then(Value::as_u64).map(|v| v as usize);
            let min_strength = args.get("min_strength").and_then(Value::as_f64);
            let source = if s("source").is_empty() {
                "mcp".to_string()
            } else {
                s("source")
            };
            let situation = Situation {
                query: (!query.is_empty()).then_some(query.as_str()),
                last_error: last_error.as_deref(),
                recent_actions: &recent_actions,
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
            Ok(json!({
                // 显式声明:直觉仅供参考,不是精准答案。随每个 verdict 返给 agent。
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
            }))
        }
        "innate_record" => {
            let trace_id = s("trace_id");
            let outcome = so("outcome");
            let used = arr("used");
            let used_ref: Option<&[String]> = args
                .get("used")
                .and_then(Value::as_array)
                .map(|_| used.as_slice());
            let fb_up = arr("feedback_up");
            let fb_up_ref: Option<&[String]> = if fb_up.is_empty() { None } else { Some(&fb_up) };
            let fb_down = arr("feedback_down");
            let fb_down_ref: Option<&[String]> = if fb_down.is_empty() {
                None
            } else {
                Some(&fb_down)
            };
            let source = if s("source").is_empty() {
                "mcp".to_string()
            } else {
                s("source")
            };
            let used_attribution = if s("used_attribution").is_empty() {
                "explicit".to_string()
            } else {
                s("used_attribution")
            };
            let feedback_kind = if s("feedback_kind").is_empty() {
                "user".to_string()
            } else {
                s("feedback_kind")
            };
            let query = so("query");
            let output = so("output");
            let output_summary = so("output_summary");
            let feedback_actor = so("feedback_actor");
            let feedback_reason = so("feedback_reason");
            let nomination = so("nomination");
            let task_state = so("task_state");
            kb.record(RecordParams {
                trace_id: &trace_id,
                query: query.as_deref(),
                output: output.as_deref(),
                output_summary: output_summary.as_deref(),
                outcome: outcome.as_deref(),
                used: used_ref,
                used_attribution: &used_attribution,
                used_complete: Some(b("used_complete", true)),
                feedback_up: fb_up_ref,
                feedback_down: fb_down_ref,
                feedback_kind: &feedback_kind,
                feedback_actor: feedback_actor.as_deref(),
                feedback_reason: feedback_reason.as_deref(),
                nomination: nomination.as_deref(),
                priority: n("priority", 0),
                task_state: task_state.as_deref(),
                source: &source,
                verdict_heeded: b("verdict_heeded", false),
            })?;
            Ok(json!({"ok": true}))
        }
        "innate_add" => {
            let content = s("content");
            let kind = if s("kind").is_empty() {
                "note".to_string()
            } else {
                s("kind")
            };
            let source = if s("source").is_empty() {
                "agent".to_string()
            } else {
                s("source")
            };
            let dep_kind = if s("dep_kind").is_empty() {
                "hard".to_string()
            } else {
                s("dep_kind")
            };
            let deps: Vec<(String, String)> = arr("depends_on")
                .into_iter()
                .map(|d| (d, dep_kind.clone()))
                .collect();
            let id = kb.add_with_deps(
                &content,
                &kind,
                so("trigger_desc").as_deref(),
                so("anti_trigger_desc").as_deref(),
                &source,
                so("skill_name").as_deref(),
                &deps,
            )?;
            Ok(json!({"chunk_id": id}))
        }
        "innate_spark" => {
            let content = s("content");
            let id = kb.spark(
                &content,
                so("trigger_desc").as_deref(),
                so("anti_trigger_desc").as_deref(),
            )?;
            Ok(json!({"chunk_id": id}))
        }
        "innate_inspect" => kb.inspect(),
        "innate_evolve" => {
            let trigger = if s("trigger").is_empty() {
                "manual".to_string()
            } else {
                s("trigger")
            };
            if b("rebuild_embeddings", false) {
                let rebuilt = kb.rebuild_embeddings()?;
                let evolve = kb.evolve(&trigger)?;
                return Ok(json!({"rebuilt_embeddings": rebuilt, "evolve": evolve}));
            }
            kb.evolve(&trigger)
        }
        "innate_approve" => {
            kb.approve(&s("chunk_id"))?;
            Ok(json!({"ok": true}))
        }
        "innate_archive" => {
            let reason = s("reason");
            let reason = if reason.is_empty() { "stale" } else { &reason };
            kb.archive(&s("chunk_id"), reason)?;
            Ok(json!({"ok": true}))
        }
        "innate_invalidate" => {
            kb.invalidate(&s("chunk_id"), &s("reason"))?;
            Ok(json!({"ok": true}))
        }
        "innate_restore" => {
            kb.restore(&s("chunk_id"))?;
            Ok(json!({"ok": true}))
        }
        "innate_mature_spark" => {
            kb.mature_spark(&s("spark_id"), &s("to"))?;
            Ok(json!({"ok": true}))
        }
        "innate_promote_spark" => {
            let to = if s("to").is_empty() {
                "note".to_string()
            } else {
                s("to")
            };
            let new_id = kb.promote_spark(&s("spark_id"), &to)?;
            Ok(json!({"chunk_id": new_id}))
        }
        "innate_drop_spark" => {
            kb.drop_spark(&s("spark_id"), &s("reason"))?;
            Ok(json!({"ok": true}))
        }
        "innate_backup" => {
            let db_path = kb.storage.db_path.clone();
            dispatch_backup(&s("action"), b("force", false), &db_path)
        }
        _ => Err(crate::errors::InnateError::Other(format!(
            "unknown tool: {name}"
        ))),
    }
}

fn tool_schema(name: &str) -> Value {
    match name {
        "innate_recall" => json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Search query"},
                "budget": {"type": "integer", "description": "Token budget (default 6000)"},
                "top": {"type": "integer", "description": "Max results"},
                "include_sparks": {"type": "boolean"},
                "expand_deps": {"type": "string", "enum": ["false","direct","closure"], "description": "Dependency expansion: false (default) | direct | closure"},
                "source": {"type": "string", "enum": ["mcp","sdk","cli","hook","daemon","augmented"]},
                "min_score": {"type": "number", "description": "Relevance gate: drop candidates whose fused score is below this value (omit to disable)"}
            },
            "required": ["query"]
        }),
        "innate_appraise" => json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Explicit question/instruction (optional)"},
                "last_error": {"type": "string", "description": "Current or last error text"},
                "recent_actions": {"type": "array", "items": {"type": "string"}, "description": "Last few actions taken"},
                "stage": {"type": "string", "description": "Task stage, e.g. merge | implement | review"},
                "file_context": {"type": "string", "description": "File type/path summary in scope"},
                "candidate": {"type": "string", "description": "Candidate answer under judgement (sanitized, never echoed back)"},
                "top": {"type": "integer"},
                "min_strength": {"type": "number"},
                "source": {"type": "string", "enum": ["mcp","sdk","cli","hook","daemon","augmented"]}
            }
        }),
        "innate_record" => json!({
            "type": "object",
            "properties": {
                "trace_id": {"type": "string"},
                "query": {"type": "string", "description": "Original query from the corresponding recall"},
                "output": {"type": "string", "description": "Raw task output (optional, for distillation)"},
                "outcome": {"type": "string", "enum": ["ok","fail","unknown"]},
                "used": {"type": "array", "items": {"type": "string"}},
                "used_attribution": {"type": "string", "enum": ["explicit","cited","inferred"]},
                "used_complete": {"type": "boolean", "description": "Whether used exhaustively lists all selected chunks (default true)"},
                "feedback_up": {"type": "array", "items": {"type": "string"}},
                "feedback_down": {"type": "array", "items": {"type": "string"}},
                "feedback_kind": {"type": "string", "enum": ["user","judge"]},
                "feedback_actor": {"type": "string"},
                "feedback_reason": {"type": "string"},
                "task_state": {"type": "string", "enum": ["recalled","running","completed","abandoned","timed_out"]},
                "output_summary": {"type": "string"},
                "nomination": {"type": "string"},
                "priority": {"type": "integer"},
                "verdict_heeded": {"type": "boolean", "description": "Set when this trace came from an appraise whose caution was heeded (action avoided): the outcome is counterfactual and is excluded from the critic's calibration."},
                "source": {"type": "string", "enum": ["mcp","sdk","cli","hook","daemon","augmented"]}
            },
            "required": ["trace_id"]
        }),
        "innate_add" => json!({
            "type": "object",
            "properties": {
                "content": {"type": "string"},
                "kind": {"type": "string", "enum": ["note","skill"]},
                "trigger_desc": {"type": "string"},
                "anti_trigger_desc": {"type": "string"},
                "source": {"type": "string", "enum": ["chat","manual","doc","agent"]},
                "skill_name": {"type": "string"},
                "depends_on": {"type": "array", "items": {"type": "string"}, "description": "Chunk ids this chunk depends on"},
                "dep_kind": {"type": "string", "enum": ["hard","soft"], "description": "Dependency kind for depends_on (default hard)"}
            },
            "required": ["content"]
        }),
        "innate_spark" => json!({
            "type": "object",
            "properties": {
                "content": {"type": "string"},
                "trigger_desc": {"type": "string"},
                "anti_trigger_desc": {"type": "string"}
            },
            "required": ["content"]
        }),
        "innate_inspect" => json!({"type": "object", "properties": {}}),
        "innate_evolve" => json!({
            "type": "object",
            "properties": {
                "trigger": {"type": "string", "enum": ["manual","scheduled","threshold"]},
                "rebuild_embeddings": {"type": "boolean", "description": "Also rebuild the embedding index before evolving"}
            }
        }),
        "innate_approve" | "innate_archive" | "innate_invalidate" | "innate_restore" => json!({
            "type": "object",
            "properties": {
                "chunk_id": {"type": "string"},
                "reason": {"type": "string"}
            },
            "required": ["chunk_id"]
        }),
        "innate_mature_spark" => json!({
            "type": "object",
            "properties": {
                "spark_id": {"type": "string"},
                "to": {"type": "string", "enum": ["sprouting","incubating"]}
            },
            "required": ["spark_id", "to"]
        }),
        "innate_promote_spark" => json!({
            "type": "object",
            "properties": {
                "spark_id": {"type": "string"},
                "to": {"type": "string", "enum": ["note","skill"]}
            },
            "required": ["spark_id"]
        }),
        "innate_drop_spark" => json!({
            "type": "object",
            "properties": {
                "spark_id": {"type": "string"},
                "reason": {"type": "string"}
            },
            "required": ["spark_id"]
        }),
        "innate_backup" => json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["run", "status", "list", "prune"],
                    "description": "run=backup now, status=show last backup state, list=list R2 backups, prune=delete old backups"
                },
                "force": {
                    "type": "boolean",
                    "description": "For action=run: skip the interval check and backup immediately (default false)"
                }
            },
            "required": ["action"]
        }),
        _ => json!({"type": "object", "properties": {}}),
    }
}

// ---------------------------------------------------------------------------
// Backup dispatch (converts anyhow errors to InnateError::Other)
// ---------------------------------------------------------------------------

fn dispatch_backup(
    action: &str,
    force: bool,
    db_path: &std::path::Path,
) -> crate::errors::Result<Value> {
    use crate::backup::R2BackupService;
    use crate::errors::InnateError;

    let settings = crate::settings::load()?;
    let cfg = settings.backup.as_ref().ok_or_else(|| {
        InnateError::Other(
            "backup not configured — add a \"backup\" section with \"enable\": true \
             and \"r2\" credentials to ~/.innate/settings.json"
                .into(),
        )
    })?;
    if !cfg.enable {
        return Err(InnateError::Other(
            "R2 backup is disabled (backup.enable = false). \
             Set \"enable\": true in ~/.innate/settings.json to activate."
                .into(),
        ));
    }
    let r2_cfg = cfg.r2.as_ref().ok_or_else(|| {
        InnateError::Other("backup.r2 not configured in ~/.innate/settings.json".into())
    })?;

    match action {
        "run" => {
            if !force && !R2BackupService::needs_backup(cfg.auto_backup_interval_hours) {
                let state = R2BackupService::last_backup_state();
                return Ok(json!({
                    "ok": false,
                    "reason": "not_due",
                    "last_backup_at": state.last_backup_at,
                    "interval_hours": cfg.auto_backup_interval_hours,
                }));
            }
            let svc = R2BackupService::from_config(r2_cfg)
                .map_err(|e| InnateError::Other(e.to_string()))?;
            let result = svc
                .backup_now(db_path, cfg.retention_days, cfg.min_backups)
                .map_err(|e| InnateError::Other(e.to_string()))?;
            Ok(json!({
                "ok": true,
                "key": result.key,
                "size_bytes": result.size_bytes,
                "pruned": result.prune.deleted,
                "kept": result.prune.kept,
                "protected_by_min": result.prune.protected_by_min,
            }))
        }
        "status" => {
            let state = R2BackupService::last_backup_state();
            Ok(json!({
                "bucket": r2_cfg.bucket,
                "last_backup_at": state.last_backup_at,
                "last_backup_key": state.last_backup_key,
                "backup_due": R2BackupService::needs_backup(cfg.auto_backup_interval_hours),
                "interval_hours": cfg.auto_backup_interval_hours,
                "retention_days": cfg.retention_days,
                "min_backups": cfg.min_backups,
            }))
        }
        "list" => {
            let svc = R2BackupService::from_config(r2_cfg)
                .map_err(|e| InnateError::Other(e.to_string()))?;
            let backups = svc
                .list_backups()
                .map_err(|e| InnateError::Other(e.to_string()))?;
            Ok(json!({"backups": backups}))
        }
        "prune" => {
            let svc = R2BackupService::from_config(r2_cfg)
                .map_err(|e| InnateError::Other(e.to_string()))?;
            let result = svc
                .prune_old_backups(cfg.retention_days, cfg.min_backups)
                .map_err(|e| InnateError::Other(e.to_string()))?;
            Ok(json!({
                "deleted": result.deleted,
                "kept": result.kept,
                "protected_by_min": result.protected_by_min,
            }))
        }
        other => Err(InnateError::Other(format!(
            "unknown backup action '{other}'; valid: run, status, list, prune"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Daemon auto-start
// ---------------------------------------------------------------------------

fn default_pid_file() -> PathBuf {
    crate::paths::daemon_pid_path()
}

/// If `settings.daemon.auto_start` is true and watch_dirs are configured, start the
/// daemon in the background if it is not already running. Failures are silently ignored
/// so the MCP server always starts even when the daemon can't be spawned.
fn maybe_auto_start_daemon(db_path: &std::path::Path) {
    // Best-effort: a corrupt config simply means "no auto-start" rather than
    // blocking MCP startup (errors here are intentionally swallowed).
    let s = crate::settings::load().unwrap_or_default();
    match &s.daemon {
        Some(c) if c.auto_start => {}
        _ => return,
    };
    let watch_dirs = crate::settings::resolved_watch_dirs(&s);
    if watch_dirs.is_empty() {
        return;
    }

    let pid_file = default_pid_file();
    if crate::daemon::is_running(&pid_file) {
        return;
    }

    // Spawn `innate --db <path> daemon start --watch ...` as a detached child process.
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = std::process::Command::new(&exe);
    // --db is a global flag that must come before the subcommand.
    cmd.arg("--db").arg(db_path);
    cmd.arg("daemon").arg("start");
    for dir in &watch_dirs {
        cmd.arg("--watch").arg(dir);
    }
    // Redirect stdio away from the MCP stdio channel.
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    // Use setsid on Linux so the daemon is in its own process group and won't
    // receive signals sent to the MCP server's process group.
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }

    let _ = cmd.spawn(); // fire-and-forget
}
