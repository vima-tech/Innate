//! Action-time recall (schema 5.0): `innate hook pre-tool` (PreToolUse) and
//! `innate hook tool-failure` (PostToolUseFailure).
//!
//! Knowledge about *how to do things* is needed while the agent works — right
//! before a command runs, or right after one fails — not only when the user
//! types. On 2026-09-23 an agent hung for ten minutes on
//! `until ! pgrep -f '…'`, which matched its own shell; the library held that
//! exact lesson, but recall only ran on the user's prompt.
//!
//! These hooks match the command or error text against rule signals locally
//! (no network, no embedding) and inject at most one rule, at most once per
//! session, only on a strong match. The injection is recorded as `shown`; the
//! Stop hook then attaches what happened next, and the offline judge turns it
//! into a verdict. Hooks must never break the session: every error is
//! swallowed by the caller.

use std::path::Path;

use serde_json::{json, Value};

use crate::action_match::{best_match, RuleSignals};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActionKind {
    /// Before a Bash command runs: match the command.
    PreTool,
    /// After a tool call failed: match the error (and the command).
    ToolFailure,
}

impl ActionKind {
    fn channel(self) -> &'static str {
        match self {
            ActionKind::PreTool => "pre_tool",
            ActionKind::ToolFailure => "tool_error",
        }
    }
    fn event(self) -> &'static str {
        match self {
            ActionKind::PreTool => "PreToolUse",
            ActionKind::ToolFailure => "PostToolUseFailure",
        }
    }
}

fn text_of(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// The text to match for this event.
pub(crate) fn match_text(kind: ActionKind, payload: &Value) -> String {
    let command = text_of(payload.pointer("/tool_input/command"));
    match kind {
        ActionKind::PreTool => command,
        ActionKind::ToolFailure => {
            // The failure payload's field name has varied between releases.
            let error = ["tool_error", "error", "tool_output", "tool_response"]
                .iter()
                .map(|k| text_of(payload.get(*k)))
                .find(|t| !t.trim().is_empty())
                .unwrap_or_default();
            let error: String = error.chars().take(4000).collect();
            format!("{command}\n{error}")
        }
    }
}

/// Build the injected context, or `None` when nothing fits.
pub(crate) fn respond(
    kb: &crate::kb::KnowledgeBase,
    kind: ActionKind,
    payload: &Value,
) -> anyhow::Result<Option<Value>> {
    if kind == ActionKind::PreTool
        && payload.get("tool_name").and_then(Value::as_str) != Some("Bash")
    {
        return Ok(None);
    }
    let text = match_text(kind, payload);
    if text.trim().is_empty() {
        return Ok(None);
    }
    let rules: Vec<RuleSignals> = kb
        .storage
        .rule_signals()?
        .into_iter()
        .map(|(id, state, signals)| RuleSignals::from_row(id, &state, &signals))
        .collect();
    let Some((rule, hits)) = best_match(&text, &rules) else {
        return Ok(None);
    };
    let session = payload.get("session_id").and_then(Value::as_str);
    if let Some(s) = session {
        if kb.storage.shown_in_session(&rule.id, s)? {
            return Ok(None);
        }
    }
    let Some(chunk) = kb.storage.get_chunk(&rule.id)? else {
        return Ok(None);
    };
    let content = chunk.get("content").and_then(Value::as_str).unwrap_or("");
    let project = payload
        .get("cwd")
        .and_then(Value::as_str)
        .map(Path::new)
        .and_then(crate::project::project_of);
    let tool_use_id = payload.get("tool_use_id").and_then(Value::as_str);
    kb.mark_shown(
        &rule.id,
        kind.channel(),
        session,
        project.as_deref(),
        tool_use_id,
    )?;

    let status = if rule.active {
        "已验证规则"
    } else {
        "候选规则·待验证"
    };
    let when = match kind {
        ActionKind::PreTool => "即将执行的命令",
        ActionKind::ToolFailure => "刚才失败的命令",
    };
    let context = format!(
        "<innate-rule>\nInnate：{when}命中了一条{status}（匹配信号：{}）。\n- [{}] {content}\n\
         如果你据此调整了做法，收尾调用 innate_record 时在 verdicts 里给出判定 \
         （supported / contradicted / irrelevant）并写明实际观察到的结果。\n</innate-rule>",
        hits.join("、"),
        rule.id
    );
    Ok(Some(json!({
        "hookSpecificOutput": {
            "hookEventName": kind.event(),
            "additionalContext": context,
        }
    })))
}

pub(crate) fn run(db_path: &Path, kind: ActionKind) -> anyhow::Result<()> {
    use std::io::Read;
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let payload: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
    let kb = crate::open_kb(db_path)?;
    if let Some(out) = respond(&kb, kind, &payload)? {
        println!("{out}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kb_with_rule(state: &str) -> (crate::kb::KnowledgeBase, tempfile::NamedTempFile, String) {
        let f = tempfile::NamedTempFile::new().unwrap();
        let kb = crate::kb::KnowledgeBase::open(f.path()).unwrap();
        let id = kb
            .add(
                "用 pgrep -f 等待进程退出会匹配到调用者自己的命令行；改用 pgrep -x 或锚定模式。",
                "note",
                Some("t"),
                None,
                "manual",
                None,
            )
            .unwrap();
        kb.storage
            .conn_execute_count(
                "UPDATE chunks SET state=?1, signals='[\"pgrep -f\",\"进程自匹配\"]' WHERE id=?2",
                rusqlite::params![state, id],
            )
            .unwrap();
        (kb, f, id)
    }

    fn pre(cmd: &str, session: &str) -> Value {
        json!({"tool_name":"Bash","tool_input":{"command":cmd},"session_id":session,
               "cwd":"/work/shop","tool_use_id":"toolu_1"})
    }

    #[test]
    fn matching_command_injects_the_rule_once_per_session() {
        let (kb, _f, id) = kb_with_rule("active");
        let out = respond(
            &kb,
            ActionKind::PreTool,
            &pre("until ! pgrep -f 'x'; do sleep 2; done", "s1"),
        )
        .unwrap()
        .expect("a strong match is injected");
        let ctx = out["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(ctx.contains(&id) && ctx.contains("已验证规则") && ctx.contains("pgrep -f"));
        assert_eq!(out["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        let shown = kb.storage.shown_awaiting_observation("s1").unwrap();
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0]["channel"], "pre_tool");
        assert_eq!(shown[0]["tool_use_id"], "toolu_1");
        // Second time in the same session: silent.
        assert!(respond(&kb, ActionKind::PreTool, &pre("pgrep -f y", "s1"))
            .unwrap()
            .is_none());
        // A new session sees it again.
        assert!(respond(&kb, ActionKind::PreTool, &pre("pgrep -f y", "s2"))
            .unwrap()
            .is_some());
    }

    #[test]
    fn unrelated_commands_and_non_bash_tools_stay_silent() {
        let (kb, _f, _) = kb_with_rule("active");
        assert!(
            respond(&kb, ActionKind::PreTool, &pre("cargo test --release", "s1"))
                .unwrap()
                .is_none()
        );
        let edit =
            json!({"tool_name":"Edit","tool_input":{"file_path":"pgrep -f"},"session_id":"s1"});
        assert!(respond(&kb, ActionKind::PreTool, &edit).unwrap().is_none());
    }

    #[test]
    fn failure_output_is_matched_and_candidates_are_labelled() {
        let (kb, _f, _) = kb_with_rule("pending");
        let payload = json!({"tool_name":"Bash","tool_input":{"command":"./wait.sh"},
            "tool_error":"script hung: pgrep -f matched its own shell","session_id":"s9"});
        let out = respond(&kb, ActionKind::ToolFailure, &payload)
            .unwrap()
            .unwrap();
        let ctx = out["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(ctx.contains("候选规则·待验证"));
        assert_eq!(
            out["hookSpecificOutput"]["hookEventName"],
            "PostToolUseFailure"
        );
    }
}
