//! Prompts and response parsing for the living-knowledge rule pipeline
//! (schema 5.0): writing a rule from episodes or a nomination, judging an
//! observation, revising a contradicted rule, and deriving signals.
//!
//! The model only *writes* and *reads*. What becomes a rule is decided before
//! the call (recurrence across sessions, or an explicit nomination); what counts
//! as validation is guarded after it (a verdict must quote the observation
//! verbatim). See docs/Innate-活知识库重构-整体改进方案-v1.md §4.

use serde_json::Value;

/// Version stamped on chunks written through these prompts.
pub const PROMPT_VERSION: &str = "5";

const RULE_RULES: &str = r#"A rule is knowledge a future agent can ACT on in a DIFFERENT project or task:
- signals: 2-8 concrete, searchable things that show the rule applies right now: exact
  error messages or codes, command names with flags, API / library / config names,
  visible symptoms. Copy them verbatim from the material. They are matched literally
  against shell commands and error output, so prefer distinctive tokens
  ("pgrep -f", "UNIQUE constraint failed") over generic words ("error", "build").
- content: when it applies -> what to do -> how to tell it worked -> when NOT to apply it.
  Use general wording: no project or repository names, business data, customer names,
  commit ids, dates or one-off values. Keep the technical names that decide whether
  the rule applies.
- Write skill_name, content and trigger_desc in the same language as the material.
- Return [] when the material holds no lesson that would change what a competent agent
  does elsewhere. Status reports, "task finished", project facts, one-off business
  decisions and platitudes ("verify your work", "be careful") are not rules.
- Never include secrets, credentials or personal data."#;

const RULE_SCHEMA: &str = r#"Output only a JSON array with 0, 1 or at most 2 items. Each item:
{"skill_name": "<1-3 word label>", "content": "<the rule>", "signals": ["<signal>", "..."],
 "trigger_desc": "<short phrase a future agent would search for>",
 "anti_trigger_desc": "<when not to apply it, or null>"}"#;

fn field<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("").trim()
}

/// Several episodes from different sessions that share a signal or a meaning.
/// Projects are shown as anonymous labels so no project name leaks into the rule.
pub fn rule_from_episodes(episodes: &[Value]) -> String {
    let mut projects: Vec<&str> = Vec::new();
    let mut blocks = Vec::new();
    for (i, e) in episodes.iter().enumerate() {
        let project = field(e, "project");
        let label = if project.is_empty() {
            "unknown".to_string()
        } else {
            let idx = projects
                .iter()
                .position(|p| *p == project)
                .unwrap_or_else(|| {
                    projects.push(project);
                    projects.len() - 1
                });
            format!("P{}", idx + 1)
        };
        let mut block = format!(
            "Episode {} ({}, project {}):\n{}",
            i + 1,
            field(e, "kind"),
            label,
            field(e, "trigger_text")
        );
        for (name, key) in [("Attempts", "attempts"), ("Resolution", "resolution")] {
            let text = field(e, key);
            if !text.is_empty() {
                block.push_str(&format!("\n{name}:\n{text}"));
            }
        }
        blocks.push(block);
    }
    format!(
        "You distil procedural knowledge from real work. The episodes below happened in \
         separate sessions and share a signal or a situation. Find the ONE lesson they have \
         in common, if any.\n\n{}\n\n{RULE_RULES}\n\n{RULE_SCHEMA}",
        blocks.join("\n\n")
    )
}

/// A single log an agent or user explicitly nominated as worth keeping.
pub fn rule_from_nomination(context: &str) -> String {
    format!(
        "You distil procedural knowledge from real work. An agent or user nominated the \
         interaction below as worth keeping. Turn it into a rule only if it passes the bar.\n\n\
         {context}\n\n{RULE_RULES}\n\n{RULE_SCHEMA}"
    )
}

/// Did the rule apply, and did what happened support or contradict it?
pub fn judge(rule: &str, observation: &str) -> String {
    format!(
        "A rule was shown to an agent. Below is what the agent did next and what happened.\n\n\
         Rule:\n{rule}\n\nWhat happened next (commands and their results):\n{observation}\n\n\
         Decide:\n\
         - applied: did the agent act as the rule says (true/false)?\n\
         - verdict: \"supported\" if the rule applied and the outcome matched what it predicts; \
         \"contradicted\" if the rule applied (or was followed) and the outcome shows it is \
         wrong or incomplete; \"irrelevant\" if the rule did not fit this situation; \
         \"unknown\" if the outcome cannot tell.\n\
         - quote: for supported or contradicted, copy the exact text from \"What happened \
         next\" that shows it (verbatim, at most 200 characters).\n\n\
         Output only JSON: {{\"applied\": true, \"verdict\": \"...\", \"quote\": \"...\"}}"
    )
}

/// A rule with open counterexamples: narrow it, retire it, or keep it.
pub fn revise(rule: &str, signals: &str, counterexamples: &[String]) -> String {
    format!(
        "A rule was contradicted in practice. Decide what to do.\n\nRule:\n{rule}\n\
         Signals: {signals}\n\nCounterexamples (what actually happened):\n{}\n\n\
         Choose one action:\n\
         - \"revise\": the rule is useful but its action or conditions must change; return the \
         corrected rule.\n\
         - \"retire\": the rule is wrong and should not be recommended.\n\
         - \"keep\": the counterexamples are misuse outside the rule's stated conditions; the \
         rule stands as written.\n\n\
         {RULE_RULES}\n\n\
         Output only JSON: {{\"action\": \"revise|retire|keep\", \"reason\": \"<one sentence>\", \
         \"rule\": <for revise: one item in this shape, else null>}}\n{RULE_SCHEMA}",
        counterexamples.join("\n---\n")
    )
}

/// Concrete signals for a rule written before 5.0.
pub fn signals_for(rule: &str) -> String {
    format!(
        "List the concrete, searchable signals that show this rule applies: exact error \
         messages or codes, command names with flags, API / library / config names, visible \
         symptoms. They are matched literally against shell commands and error output, so \
         use distinctive tokens and copy technical names exactly. Return [] if the rule has \
         no such signals.\n\nRule:\n{rule}\n\nOutput only a JSON array of 0-8 strings."
    )
}

/// Extract the first JSON value (array or object) from a model reply, tolerating
/// code fences and surrounding prose.
pub fn extract_json(raw: &str) -> &str {
    let t = raw.trim();
    let start = t.find(['[', '{']).unwrap_or(0);
    let open = t[start..].chars().next().unwrap_or('[');
    let close = if open == '{' { '}' } else { ']' };
    let end = t.rfind(close).map(|i| i + 1).unwrap_or(t.len());
    if end > start {
        &t[start..end]
    } else {
        t
    }
}

/// A rule as the model wrote it, before it becomes a chunk.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RuleDraft {
    pub skill_name: Option<String>,
    pub content: String,
    pub signals: Vec<String>,
    pub trigger_desc: Option<String>,
    pub anti_trigger_desc: Option<String>,
}

fn opt_str(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("null"))
        .map(str::to_string)
}

pub fn draft_from(v: &Value) -> Option<RuleDraft> {
    let content = opt_str(v, "content")?;
    let signals = v
        .get("signals")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .take(8)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Some(RuleDraft {
        skill_name: opt_str(v, "skill_name")
            .map(|s| s.split_whitespace().take(3).collect::<Vec<_>>().join(" ")),
        content,
        signals,
        trigger_desc: opt_str(v, "trigger_desc"),
        anti_trigger_desc: opt_str(v, "anti_trigger_desc"),
    })
}

pub fn parse_rules(raw: &str) -> Result<Vec<RuleDraft>, String> {
    let v: Value = serde_json::from_str(extract_json(raw)).map_err(|e| e.to_string())?;
    let items = match v {
        Value::Array(items) => items,
        obj @ Value::Object(_) => vec![obj],
        _ => return Err("expected a JSON array".into()),
    };
    Ok(items.iter().filter_map(draft_from).take(2).collect())
}

/// A judge verdict after the verbatim-quote guard.
#[derive(Debug, Clone, PartialEq)]
pub struct Judgement {
    pub verdict: &'static str,
    pub quote: Option<String>,
}

/// Parse a judge reply. `supported` / `contradicted` survive only when the
/// quote appears verbatim in the observation — the model links evidence, it
/// cannot invent it. Anything else degrades to `unknown`.
pub fn parse_judgement(raw: &str, observation: &str) -> Result<Judgement, String> {
    let v: Value = serde_json::from_str(extract_json(raw)).map_err(|e| e.to_string())?;
    let verdict = match v.get("verdict").and_then(Value::as_str).unwrap_or("") {
        "supported" => "supported",
        "contradicted" => "contradicted",
        "irrelevant" => "irrelevant",
        _ => "unknown",
    };
    let quote = opt_str(&v, "quote");
    let grounded = quote
        .as_deref()
        .is_some_and(|q| q.chars().count() >= 3 && observation.contains(q));
    let verdict = match verdict {
        "supported" | "contradicted" if !grounded => "unknown",
        "supported" if v.get("applied").and_then(Value::as_bool) == Some(false) => "irrelevant",
        other => other,
    };
    Ok(Judgement {
        verdict,
        quote: if grounded { quote } else { None },
    })
}

#[derive(Debug, Clone, PartialEq)]
pub enum Revision {
    Revise(RuleDraft, String),
    Retire(String),
    Keep(String),
}

pub fn parse_revision(raw: &str) -> Result<Revision, String> {
    let v: Value = serde_json::from_str(extract_json(raw)).map_err(|e| e.to_string())?;
    let reason = opt_str(&v, "reason").unwrap_or_default();
    match v.get("action").and_then(Value::as_str).unwrap_or("") {
        "revise" => v
            .get("rule")
            .and_then(draft_from)
            .map(|d| Revision::Revise(d, reason))
            .ok_or_else(|| "revise without a rule".to_string()),
        "retire" => Ok(Revision::Retire(reason)),
        "keep" => Ok(Revision::Keep(reason)),
        other => Err(format!("unknown action {other:?}")),
    }
}

pub fn parse_signals(raw: &str) -> Result<Vec<String>, String> {
    let v: Value = serde_json::from_str(extract_json(raw)).map_err(|e| e.to_string())?;
    Ok(v.as_array()
        .ok_or("expected a JSON array")?
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .take(8)
        .map(str::to_string)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn episode_prompt_hides_project_names() {
        let p = rule_from_episodes(&[
            json!({"kind":"struggle","project":"juvenile-guard","trigger_text":"命令: x\n报错: E1"}),
            json!({"kind":"struggle","project":"sustain","trigger_text":"命令: y\n报错: E1"}),
        ]);
        assert!(!p.contains("juvenile-guard") && !p.contains("sustain"));
        assert!(p.contains("project P1") && p.contains("project P2"));
        assert!(p.contains("same language as the material"));
    }

    #[test]
    fn rules_parse_from_fenced_replies_and_empty_means_no_rule() {
        let raw = "```json\n[{\"skill_name\":\"进程匹配\",\"content\":\"...\",\"signals\":[\"pgrep -f\",\"\"]}]\n```";
        let rules = parse_rules(raw).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].signals, vec!["pgrep -f"]);
        assert!(parse_rules("[]").unwrap().is_empty());
    }

    #[test]
    fn judgement_without_a_verbatim_quote_is_unknown() {
        let obs = "[失败] pgrep -f x\nCommand timed out";
        let ok = parse_judgement(
            r#"{"applied":true,"verdict":"contradicted","quote":"Command timed out"}"#,
            obs,
        )
        .unwrap();
        assert_eq!(ok.verdict, "contradicted");
        let invented = parse_judgement(
            r#"{"applied":true,"verdict":"supported","quote":"all tests passed"}"#,
            obs,
        )
        .unwrap();
        assert_eq!(invented.verdict, "unknown");
        assert_eq!(invented.quote, None);
        let not_applied = parse_judgement(
            r#"{"applied":false,"verdict":"supported","quote":"pgrep -f x"}"#,
            obs,
        )
        .unwrap();
        assert_eq!(not_applied.verdict, "irrelevant");
    }

    #[test]
    fn revision_actions_parse() {
        let r = parse_revision(
            r#"{"action":"revise","reason":"r","rule":{"content":"new","signals":["a b"]}}"#,
        )
        .unwrap();
        assert!(matches!(r, Revision::Revise(ref d, _) if d.content == "new"));
        assert_eq!(
            parse_revision(r#"{"action":"retire","reason":"wrong"}"#).unwrap(),
            Revision::Retire("wrong".into())
        );
        assert!(parse_revision(r#"{"action":"revise","rule":null}"#).is_err());
    }
}
