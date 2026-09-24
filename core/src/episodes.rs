//! Deterministic episode extraction from an agent transcript (schema 5.0).
//!
//! An *episode* is experience material — the thing a rule is later born from
//! and judged against. Two kinds are captured, both without an LLM:
//!
//! * **struggle** — a shell command fails, the agent tries again, and a later
//!   command of the same program succeeds. The failure, the attempts and the
//!   recovery are the part of a session worth learning from; the final
//!   "done, all green" reply (what the Stop hook used to distil) is not.
//! * **correction** — the user tells the agent it got something wrong. This is
//!   how non-coding sessions (outreach, documents, quotes) produce material.
//!
//! The Claude Code transcript (`.jsonl`) is an internal format that may change
//! between releases, so every accessor here is defensive: a line or block that
//! does not have the expected shape is skipped, never an error.

use serde_json::Value;

use crate::entities::extract_entities;
use crate::utils::{content_hash, redact_secrets};

/// One tool call with its result, in transcript order.
#[derive(Debug, Clone, Default)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Bash command, edited file path, or a short rendering of the input.
    pub input: String,
    pub ts: Option<String>,
    pub result: String,
    pub is_error: bool,
}

#[derive(Debug, Clone)]
pub enum Event {
    /// Text the human typed (hook injections and system tags excluded).
    User {
        uuid: String,
        ts: Option<String>,
        text: String,
    },
    Assistant {
        text: String,
    },
    Tool(ToolCall),
}

#[derive(Debug, Clone, Default)]
pub struct Transcript {
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub events: Vec<Event>,
}

/// An episode before it is stored.
#[derive(Debug, Clone, PartialEq)]
pub struct RawEpisode {
    pub id: String,
    pub kind: &'static str,
    pub ts: Option<String>,
    pub trigger: String,
    pub attempts: Option<String>,
    pub resolution: Option<String>,
    pub signals: Vec<String>,
}

const TRIGGER_MAX: usize = 900;
const RESULT_MAX: usize = 300;
const CMD_MAX: usize = 300;
const ATTEMPT_MAX: usize = 200;
/// How many later tool calls may pass before a failure counts as unrecovered.
const RECOVERY_WINDOW: usize = 15;

fn clip(text: &str, max: usize) -> String {
    let text = redact_secrets(text.trim()).0;
    if text.chars().count() <= max {
        return text;
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

/// Drop ANSI escape sequences (`ESC [ … letter`): colored tool output would
/// otherwise put `[31m`-style noise into error signatures.
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for d in chars.by_ref() {
                    if d.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

fn block_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn render_input(name: &str, input: &Value) -> String {
    if let Some(cmd) = input.get("command").and_then(Value::as_str) {
        return cmd.to_string();
    }
    if let Some(path) = input.get("file_path").and_then(Value::as_str) {
        return format!("{name} {path}");
    }
    let raw = input.to_string();
    format!("{name} {}", raw.chars().take(120).collect::<String>())
}

/// Human-typed text only: hook output, reminders, notifications and slash
/// command echoes all arrive as user-role text wrapped in tags.
fn is_human_text(text: &str) -> bool {
    let t = text.trim_start();
    !t.is_empty() && !t.starts_with('<') && !t.starts_with("[Request interrupted")
}

/// Parse a `.jsonl` transcript into ordered events.
pub fn parse(transcript: &str) -> Transcript {
    let mut out = Transcript::default();
    let mut pending: Vec<ToolCall> = Vec::new();
    for line in transcript.lines() {
        let Ok(m) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if out.session_id.is_none() {
            out.session_id = m
                .get("sessionId")
                .or_else(|| m.get("session_id"))
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        if out.cwd.is_none() {
            out.cwd = m.get("cwd").and_then(Value::as_str).map(str::to_string);
        }
        let ts = m
            .get("timestamp")
            .and_then(Value::as_str)
            .map(str::to_string);
        let role = m.pointer("/message/role").and_then(Value::as_str);
        let content = m.pointer("/message/content");
        match role {
            Some("assistant") => {
                let Some(Value::Array(blocks)) = content else {
                    continue;
                };
                for b in blocks {
                    match b.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            if let Some(t) = b.get("text").and_then(Value::as_str) {
                                out.events.push(Event::Assistant {
                                    text: t.to_string(),
                                });
                            }
                        }
                        Some("tool_use") => {
                            let name = b.get("name").and_then(Value::as_str).unwrap_or("");
                            let input = b.get("input").cloned().unwrap_or(Value::Null);
                            pending.push(ToolCall {
                                id: b
                                    .get("id")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_string(),
                                name: name.to_string(),
                                input: render_input(name, &input),
                                ts: ts.clone(),
                                ..Default::default()
                            });
                        }
                        _ => {}
                    }
                }
            }
            Some("user") => {
                if let Some(Value::Array(blocks)) = content {
                    let mut had_result = false;
                    for b in blocks {
                        if b.get("type").and_then(Value::as_str) != Some("tool_result") {
                            continue;
                        }
                        had_result = true;
                        let id = b.get("tool_use_id").and_then(Value::as_str).unwrap_or("");
                        let pos = pending.iter().position(|c| c.id == id);
                        let mut call = pos.map(|p| pending.remove(p)).unwrap_or_else(|| ToolCall {
                            id: id.to_string(),
                            ts: ts.clone(),
                            ..Default::default()
                        });
                        call.result = strip_ansi(&block_text(b.get("content")));
                        call.is_error = b.get("is_error").and_then(Value::as_bool).unwrap_or(false);
                        out.events.push(Event::Tool(call));
                    }
                    if had_result {
                        continue;
                    }
                }
                let text = block_text(content);
                if is_human_text(&text) {
                    out.events.push(Event::User {
                        uuid: m
                            .get("uuid")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        ts,
                        text,
                    });
                }
            }
            _ => {}
        }
    }
    out
}

/// Programs that say nothing about what a command is *for*; a failing
/// `grep` and a later successful `grep` are not a recovery.
const GENERIC_PROGRAMS: &[&str] = &[
    "cd", "echo", "cat", "ls", "head", "tail", "true", "false", "set", "export", "sleep", "printf",
    "wc", "sort", "uniq", "cut", "tr", "grep", "sed", "awk", "xargs", "tee", "rm", "cp", "mv",
    "mkdir", "touch", "pwd", "which", "test", "for", "while", "do", "done", "if", "then", "fi",
    "else", "bash", "sh", "source", "until", "let", "read", "find", "ps", "kill", "pgrep",
];

/// Wrappers whose *next* word is the real program (`sudo -S systemctl …`,
/// `timeout 300 innate evolve`).
const WRAPPERS: &[&str] = &["sudo", "timeout", "env", "nohup", "time", "exec", "command"];

/// The non-generic programs a shell command runs (`cd x && cargo test | tail`
/// → `{cargo}`).
fn programs(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    for segment in command.split(['&', '|', ';', '\n', '(', ')']) {
        for word in segment.split_whitespace() {
            let is_assignment = word.contains('=') && !word.starts_with('-');
            if is_assignment || word.starts_with('-') || word.chars().all(|c| c.is_ascii_digit()) {
                continue; // FOO=bar prefix, wrapper flag or wrapper argument
            }
            let prog = word.rsplit('/').next().unwrap_or(word).to_lowercase();
            if WRAPPERS.contains(&prog.as_str()) {
                continue;
            }
            if prog
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
                && !GENERIC_PROGRAMS.contains(&prog.as_str())
                && !prog.is_empty()
                && !out.contains(&prog)
            {
                out.push(prog);
            }
            break; // only the first real word of each segment is the program
        }
    }
    out
}

/// A failure that teaches nothing: `grep` finding no match, or the user
/// declining a tool call (that is a correction, captured separately).
fn is_noise_failure(result: &str) -> bool {
    let r = result.trim();
    r == "Exit code 1"
        || r.contains("doesn't want to proceed")
        || r.contains("was rejected")
        || r.contains("user denied")
        // The harness's permission classifier blocking a call is not the
        // command failing.
        || r.contains("Permission for this action was denied")
        || r.contains("auto mode classifier")
}

const ERROR_WORDS: &[&str] = &[
    "error",
    "failed",
    "failure",
    "exception",
    "panicked",
    "not found",
    "denied",
    "refused",
    "cannot",
    "can't",
    "invalid",
    "unable",
    "timeout",
    "timed out",
    "失败",
    "错误",
    "异常",
];

/// A stable, project-free signature of an error: the first line that looks
/// like an error, lowercased, with paths, quoted names and numbers removed.
/// Two sessions hitting the same failure produce the same signature.
pub fn error_signature(result: &str) -> Option<String> {
    let line = result
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("Exit code"))
        .find(|l| {
            let low = l.to_lowercase();
            ERROR_WORDS.iter().any(|w| low.contains(w))
        })?;
    let mut out = String::new();
    let mut quote: Option<char> = None;
    for word in line.split_whitespace() {
        if word.contains('/') || word.contains('\\') {
            continue;
        }
        let mut kept = String::new();
        for c in word.chars() {
            if let Some(q) = quote {
                if c == q {
                    quote = None;
                }
                continue;
            }
            if matches!(c, '`' | '"' | '\'') {
                quote = Some(c);
                continue;
            }
            kept.push(c.to_ascii_lowercase());
        }
        // Keep digits inside codes (`E0425`, `TS2304`); mask bare numbers
        // (line numbers, counts, sizes), which differ between occurrences.
        let is_code = kept.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && kept.chars().any(|c| c.is_ascii_digit());
        if !is_code {
            kept = kept
                .chars()
                .map(|c| if c.is_ascii_digit() { '#' } else { c })
                .collect();
        }
        if !kept.is_empty() {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&kept);
        }
    }
    let out: String = out.chars().take(80).collect();
    (out.chars().count() >= 8).then(|| format!("sig:{out}"))
}

/// Discriminative signals of a piece of text: error signature plus error codes,
/// flags and code symbols. Paths are dropped — they name a project, and a signal
/// that only recurs inside one project cannot show that knowledge transfers.
pub fn signals_of(text: &str) -> Vec<String> {
    let mut out: Vec<String> = error_signature(text).into_iter().collect();
    for e in extract_entities(text, None) {
        if is_discriminative(&e.entity, e.etype) && !out.contains(&e.entity) {
            out.push(e.entity);
        }
    }
    out.truncate(12);
    out
}

/// Only shapes that name one specific thing can link two episodes: error
/// codes, code symbols with a digit / `_` / `.` / `::`, and long options.
/// Hyphenated words (`non-test`), shell keywords and short flags (`-c`) show
/// up everywhere and would glue unrelated episodes together.
fn is_discriminative(entity: &str, etype: &str) -> bool {
    match etype {
        "error" => true,
        "symbol" => {
            // Python dunders (`__init__`) appear in every traceback.
            !(entity.starts_with("__") && entity.ends_with("__"))
                && entity.chars().count() >= 4
                && (entity.chars().any(|c| c.is_ascii_digit())
                    || entity.contains('_')
                    || entity.contains('.')
                    || entity.contains("::"))
        }
        "flag" => entity.starts_with("--") && entity.chars().count() >= 6,
        _ => false,
    }
}

fn tool_calls(t: &Transcript) -> Vec<&ToolCall> {
    t.events
        .iter()
        .filter_map(|e| match e {
            Event::Tool(c) => Some(c),
            _ => None,
        })
        .collect()
}

/// Failed shell command → attempts → a later successful command of the same
/// program. Unrecovered failures are left out: they show what broke, not what
/// works.
pub fn struggles(t: &Transcript) -> Vec<RawEpisode> {
    let calls = tool_calls(t);
    let mut out = Vec::new();
    let mut i = 0;
    while i < calls.len() {
        let anchor = calls[i];
        if anchor.name != "Bash" || !anchor.is_error || is_noise_failure(&anchor.result) {
            i += 1;
            continue;
        }
        let family = programs(&anchor.input);
        if family.is_empty() {
            i += 1;
            continue;
        }
        let window = &calls[i + 1..calls.len().min(i + 1 + RECOVERY_WINDOW)];
        let recovery = window.iter().position(|c| {
            c.name == "Bash" && !c.is_error && programs(&c.input).iter().any(|p| family.contains(p))
        });
        let Some(r) = recovery else {
            i += 1;
            continue;
        };
        let recovered = window[r];
        let attempts: Vec<String> = window[..r]
            .iter()
            .filter(|c| c.name == "Bash")
            .take(6)
            .map(|c| clip(&c.input, ATTEMPT_MAX))
            .collect();
        let trigger = clip(
            &format!(
                "命令: {}\n报错: {}",
                clip(&anchor.input, CMD_MAX),
                anchor.result
            ),
            TRIGGER_MAX,
        );
        out.push(RawEpisode {
            id: content_hash(&format!(
                "{}|struggle|{}",
                t.session_id.as_deref().unwrap_or(""),
                anchor.id
            )),
            kind: "struggle",
            ts: anchor.ts.clone(),
            signals: signals_of(&anchor.result),
            trigger,
            attempts: (!attempts.is_empty()).then(|| attempts.join("\n")),
            resolution: Some(clip(
                &format!(
                    "命令: {}\n结果: {}",
                    clip(&recovered.input, CMD_MAX),
                    clip(&recovered.result, RESULT_MAX)
                ),
                TRIGGER_MAX,
            )),
        });
        // Everything up to the recovery belongs to this struggle.
        i += 1 + r + 1;
    }
    out
}

/// Phrases with which a user tells the agent it got something wrong. Kept
/// specific: bare "不是" / "no" are far too common to mean a correction.
const CORRECTION_MARKERS: &[&str] = &[
    "不对",
    "不是这样",
    "错了",
    "搞错",
    "理解错",
    "不应该",
    "应该是",
    "不要这样",
    "别这样",
    "重新来",
    "重做",
    "还是不行",
    "还是报错",
    "没有用",
    "不是我要的",
    "that's wrong",
    "that is wrong",
    "not what i",
    "you misunderstood",
    "doesn't work",
    "still broken",
    "still failing",
    "incorrect",
];

/// A human correction is short and typed; a compaction summary or a pasted
/// document that happens to contain "incorrect" is not one.
const CORRECTION_MAX_CHARS: usize = 800;

fn is_cjk(c: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&c)
}

pub fn is_correction(text: &str) -> bool {
    if text.chars().count() > CORRECTION_MAX_CHARS
        || text.trim_start().starts_with("This session is being continued")
    {
        return false;
    }
    let low = text.to_lowercase();
    CORRECTION_MARKERS.iter().any(|m| {
        // A two-character Chinese marker followed by another ideograph is part
        // of a longer word: 「不对齐」 is not 「不对」.
        let short_cjk = m.chars().count() <= 2 && m.chars().all(is_cjk);
        low.match_indices(m).any(|(i, _)| {
            !short_cjk || !low[i + m.len()..].chars().next().is_some_and(is_cjk)
        })
    })
}

/// A user correction with what the agent had said before it and what it did
/// after.
pub fn corrections(t: &Transcript) -> Vec<RawEpisode> {
    let mut out = Vec::new();
    for (i, ev) in t.events.iter().enumerate() {
        let Event::User { uuid, ts, text } = ev else {
            continue;
        };
        if !is_correction(text) {
            continue;
        }
        let before = t.events[..i].iter().rev().find_map(|e| match e {
            Event::Assistant { text } => Some(text.as_str()),
            _ => None,
        });
        let Some(before) = before else {
            continue; // a correction needs something to correct
        };
        let after = t.events[i + 1..].iter().find_map(|e| match e {
            Event::Assistant { text } => Some(text.as_str()),
            _ => None,
        });
        let tail: String = {
            let chars: Vec<char> = before.chars().collect();
            chars[chars.len().saturating_sub(300)..].iter().collect()
        };
        out.push(RawEpisode {
            id: content_hash(&format!(
                "{}|correction|{}",
                t.session_id.as_deref().unwrap_or(""),
                uuid
            )),
            kind: "correction",
            ts: ts.clone(),
            signals: signals_of(text),
            trigger: clip(
                &format!("用户纠正: {}\n之前: {}", clip(text, 400), tail),
                TRIGGER_MAX,
            ),
            attempts: None,
            resolution: after.map(|a| clip(a, RESULT_MAX)),
        });
    }
    out
}

/// What happened after a rule was shown: the tool calls that followed, each
/// with whether it failed and the start of its output. `anchor_tool_use_id`
/// (action-time) includes that call itself; otherwise calls after `after_ts`
/// (prompt-time) are taken until the next human message.
pub fn observation_after(
    t: &Transcript,
    anchor_tool_use_id: Option<&str>,
    after_ts: Option<&str>,
    max_calls: usize,
) -> Option<String> {
    let start = match anchor_tool_use_id {
        Some(id) => t
            .events
            .iter()
            .position(|e| matches!(e, Event::Tool(c) if c.id == id))?,
        None => {
            let after = after_ts?;
            t.events.iter().position(|e| match e {
                Event::Tool(c) => c.ts.as_deref().is_some_and(|ts| ts > after),
                _ => false,
            })?
        }
    };
    let mut lines = Vec::new();
    for ev in &t.events[start..] {
        match ev {
            Event::Tool(c) => {
                lines.push(format!(
                    "[{}] {}\n{}",
                    if c.is_error { "失败" } else { "成功" },
                    clip(&c.input, CMD_MAX),
                    clip(&c.result, RESULT_MAX)
                ));
                if lines.len() >= max_calls {
                    break;
                }
            }
            Event::User { .. } if anchor_tool_use_id.is_none() && !lines.is_empty() => break,
            _ => {}
        }
    }
    (!lines.is_empty()).then(|| lines.join("\n---\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn line(v: Value) -> String {
        v.to_string()
    }

    fn bash(id: &str, cmd: &str, ts: &str) -> String {
        line(
            json!({"type":"assistant","sessionId":"s1","cwd":"/w/shop","timestamp":ts,
            "message":{"role":"assistant","content":[{"type":"tool_use","id":id,"name":"Bash","input":{"command":cmd}}]}}),
        )
    }

    fn result(id: &str, out: &str, err: bool) -> String {
        line(
            json!({"type":"user","sessionId":"s1","message":{"role":"user",
            "content":[{"type":"tool_result","tool_use_id":id,"content":out,"is_error":err}]}}),
        )
    }

    fn user(uuid: &str, text: &str) -> String {
        line(
            json!({"type":"user","uuid":uuid,"sessionId":"s1","message":{"role":"user","content":text}}),
        )
    }

    fn said(text: &str) -> String {
        line(
            json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":text}]}}),
        )
    }

    #[test]
    fn failed_command_recovered_by_same_program_is_a_struggle() {
        let t = parse(
            &[
                bash("t1", "cd core && cargo build", "2026-09-24T01:00:00Z"),
                result(
                    "t1",
                    "Exit code 101\nerror[E0425]: cannot find value `x` in this scope",
                    true,
                ),
                bash("t2", "grep -n x src/lib.rs", "2026-09-24T01:00:05Z"),
                result("t2", "12: let y", false),
                bash("t3", "cd core && cargo build", "2026-09-24T01:00:09Z"),
                result("t3", "Finished release", false),
            ]
            .join("\n"),
        );
        assert_eq!(t.session_id.as_deref(), Some("s1"));
        let eps = struggles(&t);
        assert_eq!(eps.len(), 1);
        let e = &eps[0];
        assert!(e.trigger.contains("E0425"), "{}", e.trigger);
        assert!(e.attempts.as_deref().unwrap().contains("grep -n x"));
        assert!(e.resolution.as_deref().unwrap().contains("Finished"));
        assert!(
            e.signals
                .iter()
                .any(|s| s == "sig:error[e0425]: cannot find value in this scope"),
            "{:?}",
            e.signals
        );
        // Deterministic id: parsing the same transcript again yields the same row.
        assert_eq!(struggles(&t)[0].id, e.id);
    }

    #[test]
    fn grep_without_match_and_unrecovered_failures_are_not_struggles() {
        let t = parse(
            &[
                bash("a", "grep -rn nothing .", "2026-09-24T01:00:00Z"),
                result("a", "Exit code 1", true),
                bash("b", "npm run build", "2026-09-24T01:00:01Z"),
                result(
                    "b",
                    "Exit code 2\nerror TS2304: Cannot find name 'foo'",
                    true,
                ),
                bash("c", "ls", "2026-09-24T01:00:02Z"),
                result("c", "src", false),
            ]
            .join("\n"),
        );
        assert!(struggles(&t).is_empty());
    }

    #[test]
    fn user_correction_after_an_assistant_reply_is_captured() {
        let t = parse(
            &[
                user("u1", "给这家客户写第一封邮件"),
                said("已写好邮件，附上了报价单全文。"),
                user("u2", "不对，第一封邮件不要带报价，先约时间"),
                said("明白，改成只约沟通时间。"),
            ]
            .join("\n"),
        );
        let eps = corrections(&t);
        assert_eq!(eps.len(), 1);
        assert!(eps[0].trigger.contains("不要带报价"));
        assert!(eps[0].trigger.contains("附上了报价单"));
        assert_eq!(
            eps[0].resolution.as_deref(),
            Some("明白，改成只约沟通时间。")
        );
    }

    #[test]
    fn corrections_need_a_real_correction() {
        assert!(is_correction("不对，第一封邮件不要带报价"));
        assert!(is_correction("错了"));
        assert!(!is_correction("现在页面中存在各个模块稀疏，不对齐的问题"));
        assert!(!is_correction("This session is being continued from a previous conversation. The fix was incorrect."));
        assert!(!is_correction(&"很长的粘贴文档，内容里提到错了。".repeat(100)));
    }

    #[test]
    fn ansi_colors_and_generic_tokens_do_not_become_signals() {
        let colored = strip_ansi("\u{1b}[31mAttributeError\u{1b}[0m: 'NoneType' object has no attribute 'x'");
        assert_eq!(colored, "AttributeError: 'NoneType' object has no attribute 'x'");
        let s = signals_of("until-loop non-test -c -rw-r--r-- error E0425 in crate::kb::rules --no-default-features");
        assert!(s.iter().all(|x| !matches!(x.as_str(), "until-loop" | "non-test" | "-c" | "-rw-r--r--")), "{s:?}");
        assert!(s.iter().any(|x| x == "--no-default-features"), "{s:?}");
    }

    #[test]
    fn hook_injections_are_not_mistaken_for_user_text() {
        let t = parse(
            &[
                user("u1", "<innate-recall>\n- [x] 不对的格言</innate-recall>"),
                said("ok"),
            ]
            .join("\n"),
        );
        assert!(t.events.iter().all(|e| !matches!(e, Event::User { .. })));
    }

    #[test]
    fn observation_after_an_action_time_rule_reads_the_following_calls() {
        let t = parse(
            &[
                bash(
                    "p1",
                    "until ! pgrep -f 'x evolve'; do sleep 2; done",
                    "2026-09-24T01:00:00Z",
                ),
                result("p1", "Exit code 144\nCommand timed out", true),
                bash("p2", "pgrep -x innate", "2026-09-24T01:10:00Z"),
                result("p2", "", false),
            ]
            .join("\n"),
        );
        let obs = observation_after(&t, Some("p1"), None, 3).unwrap();
        assert!(obs.starts_with("[失败] until ! pgrep -f"), "{obs}");
        assert!(obs.contains("[成功] pgrep -x innate"));
        let later = observation_after(&t, None, Some("2026-09-24T01:05:00Z"), 3).unwrap();
        assert!(later.starts_with("[成功] pgrep -x"));
    }

    #[test]
    fn wrappers_and_generic_tools_are_seen_through() {
        assert_eq!(programs("sudo -S systemctl restart app"), vec!["systemctl"]);
        assert_eq!(
            programs("cd core && timeout 300 innate evolve | tail -5"),
            vec!["innate"]
        );
        assert_eq!(programs("FOO=1 npm run build"), vec!["npm"]);
        assert!(programs("grep -rn x . | head").is_empty());
    }

    #[test]
    fn error_signature_strips_project_specifics() {
        let a = error_signature(
            "error: SQLite error: UNIQUE constraint failed: evolve_requests.reason",
        )
        .unwrap();
        assert_eq!(
            a,
            "sig:error: sqlite error: unique constraint failed: evolve_requests.reason"
        );
        let b = error_signature("Error: ENOENT: no such file or directory, open '/home/a/x.json'")
            .unwrap();
        let c = error_signature("Error: ENOENT: no such file or directory, open '/srv/b/y.json'")
            .unwrap();
        assert_eq!(b, c);
    }

    #[test]
    fn secrets_in_commands_never_reach_an_episode() {
        let t = parse(
            &[
                bash(
                    "s1",
                    "echo 'hunter2x9' | sudo -S systemctl restart app",
                    "2026-09-24T01:00:00Z",
                ),
                result(
                    "s1",
                    "Exit code 1\nsudo: 密码是 hunter2x9 incorrect password attempt",
                    true,
                ),
                bash("s2", "systemctl restart app", "2026-09-24T01:00:03Z"),
                result("s2", "", false),
            ]
            .join("\n"),
        );
        for e in struggles(&t) {
            assert!(!e.trigger.contains("hunter2x9 incorrect"), "{}", e.trigger);
        }
    }
}
