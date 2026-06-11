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

use crate::kb::KnowledgeBase;

// Tool names
const TOOLS: &[(&str, &str)] = &[
    ("innate_recall",         "Call FIRST at the start of any task — retrieve relevant knowledge from the knowledge base and get a trace_id for subsequent recording."),
    ("innate_record",         "Call LAST after completing any task — close the trace_id from recall with outcome ok/fail/unknown and optional feedback."),
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
];

pub fn run_server(db_path: PathBuf) -> anyhow::Result<()> {
    let kb = Mutex::new(KnowledgeBase::open(&db_path)?);
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
            "tools/call" => handle_tool_call(&kb, &id, &params),
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

fn handle_tool_call(kb: &Mutex<KnowledgeBase>, id: &Value, params: &Value) -> Value {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    let result = {
        let kb = kb.lock().unwrap_or_else(|e| e.into_inner());
        dispatch(&kb, name, &args)
    };

    match result {
        Ok(v) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": [{"type": "text", "text": v.to_string()}],
                "isError": false
            }
        }),
        Err(e) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": [{"type": "text", "text": format!("error: {e}")}],
                "isError": true
            }
        }),
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
            Ok(json!({
                "trace_id": result.trace_id,
                "knowledge": result.knowledge,
                "sparks": result.sparks,
                "empty": result.empty,
            }))
        }
        "innate_record" => {
            let trace_id = s("trace_id");
            let outcome = so("outcome");
            let used = arr("used");
            let used_ref: Option<&[String]> = args.get("used").map(|_| used.as_slice());
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
            kb.record_detailed(
                &trace_id,
                so("query").as_deref(),
                so("output").as_deref(),
                so("output_summary").as_deref(),
                outcome.as_deref(),
                used_ref,
                &used_attribution,
                fb_up_ref,
                fb_down_ref,
                &feedback_kind,
                so("feedback_actor").as_deref(),
                so("feedback_reason").as_deref(),
                so("nomination").as_deref(),
                n("priority", 0),
                so("task_state").as_deref(),
                &source,
            )?;
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
            let id = kb.add(
                &content,
                &kind,
                so("trigger_desc").as_deref(),
                so("anti_trigger_desc").as_deref(),
                &source,
                so("skill_name").as_deref(),
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
                "source": {"type": "string", "enum": ["mcp","sdk","cli","hook","daemon","augmented"]}
            },
            "required": ["query"]
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
                "feedback_up": {"type": "array", "items": {"type": "string"}},
                "feedback_down": {"type": "array", "items": {"type": "string"}},
                "feedback_kind": {"type": "string", "enum": ["user","judge"]},
                "feedback_actor": {"type": "string"},
                "feedback_reason": {"type": "string"},
                "task_state": {"type": "string", "enum": ["recalled","running","completed","abandoned","timed_out"]},
                "output_summary": {"type": "string"},
                "nomination": {"type": "string"},
                "priority": {"type": "integer"},
                "source": {"type": "string"}
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
                "skill_name": {"type": "string"}
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
        _ => json!({"type": "object", "properties": {}}),
    }
}
