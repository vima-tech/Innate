//! Action-time matching (schema 5.0): which rule, if any, fits the shell
//! command an agent is about to run or the error it just got.
//!
//! Pure and local — no embedding, no network — because it runs before every
//! Bash call. A rule matches when one of its signals appears literally in the
//! text. Only distinctive signals count: a rule is shown at most once per call
//! and only on a strong match, so a quiet miss is always preferred to noise.
//!
//! A plain word ("pending", "coverage", "deploy") is weak evidence: it turns up
//! in unrelated commands and queries. It only counts next to another hit, never
//! alone.

/// Signals too generic to mean anything on their own.
const GENERIC: &[&str] = &[
    "error", "errors", "failed", "failure", "warning", "build", "test", "tests", "run", "git",
    "npm", "cargo", "node", "python", "python3", "bash", "sudo", "curl", "docker", "make", "http",
    "https", "file", "files", "path", "code", "true", "false", "null", "错误", "失败", "报错",
    "问题",
];

/// Commands nearly every project runs; as a signal they would fire everywhere.
const GENERIC_COMMANDS: &[&str] = &[
    "cargo test",
    "cargo build",
    "cargo run",
    "cargo clippy",
    "pytest",
    "npm test",
    "npm run build",
    "npm run dev",
    "npm install",
    "npm ci",
    "yarn build",
    "pnpm build",
    "git status",
    "git diff",
    "git commit",
    "git push",
    "git pull",
    "git log",
    "grep -n",
    "grep -r",
    "grep -rn",
    "tsc",
    "python -m pytest",
    "go test",
    "go build",
    "make test",
];

/// Minimum total length of matched signals for a match to be shown.
const MIN_SCORE: usize = 6;

/// A usable signal, lowercased, and whether it is a single plain word.
fn usable(signal: &str) -> Option<(String, bool)> {
    let raw = signal.trim().trim_start_matches("sig:").trim();
    let plain = raw.chars().all(|c| c.is_ascii_lowercase());
    let s = raw.to_lowercase();
    let n = s.chars().count();
    let cjk = s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c));
    if (cjk && n < 3)
        || (!cjk && n < 4)
        || GENERIC.contains(&s.as_str())
        || GENERIC_COMMANDS.contains(&s.as_str())
    {
        return None;
    }
    Some((s, plain))
}

/// One rule's signals, parsed from `chunks.signals` (a JSON array).
#[derive(Debug, Clone)]
pub struct RuleSignals {
    pub id: String,
    pub active: bool,
    pub signals: Vec<String>,
    /// Parallel to `signals`: true for a single plain word.
    plain: Vec<bool>,
}

impl RuleSignals {
    pub fn from_row(id: String, state: &str, signals_json: &str) -> Self {
        let signals: Vec<String> = serde_json::from_str(signals_json).unwrap_or_default();
        let (signals, plain) = signals.iter().filter_map(|s| usable(s)).unzip();
        Self {
            id,
            active: state == "active",
            signals,
            plain,
        }
    }
}

/// The best-matching rule for `text` and the signals that matched, or `None`
/// when no rule matches strongly enough. A lone plain-word hit never matches.
/// Mature rules win ties over candidates.
pub fn best_match<'a>(
    text: &str,
    rules: &'a [RuleSignals],
) -> Option<(&'a RuleSignals, Vec<String>)> {
    let hay = text.to_lowercase();
    rules
        .iter()
        .filter_map(|r| {
            let (hits, plain): (Vec<String>, Vec<bool>) = r
                .signals
                .iter()
                .zip(&r.plain)
                .filter(|(s, _)| hay.contains(s.as_str()))
                .map(|(s, p)| (s.clone(), *p))
                .unzip();
            if hits.len() == 1 && plain[0] {
                return None;
            }
            let score: usize = hits.iter().map(|s| s.chars().count()).sum();
            (score >= MIN_SCORE).then_some((r, hits, score))
        })
        .max_by(|a, b| {
            a.2.cmp(&b.2)
                .then(a.0.active.cmp(&b.0.active))
                .then(b.0.id.cmp(&a.0.id))
        })
        .map(|(r, hits, _)| (r, hits))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: &str, state: &str, signals: &[&str]) -> RuleSignals {
        RuleSignals::from_row(id.into(), state, &serde_json::to_string(signals).unwrap())
    }

    #[test]
    fn distinctive_signal_in_the_command_matches() {
        let rules = vec![
            rule("pgrep", "active", &["pgrep -f", "等待循环卡住"]),
            rule("other", "active", &["UNIQUE constraint failed"]),
        ];
        let (r, hits) = best_match(
            "until ! pgrep -f 'innate.* evolve'; do sleep 2; done",
            &rules,
        )
        .unwrap();
        assert_eq!(r.id, "pgrep");
        assert_eq!(hits, vec!["pgrep -f"]);
        let (r, _) = best_match(
            "Error: SQLite error: UNIQUE constraint failed: x.reason",
            &rules,
        )
        .unwrap();
        assert_eq!(r.id, "other");
    }

    #[test]
    fn generic_or_short_signals_never_match_alone() {
        let rules = vec![rule("noise", "active", &["error", "git", "ls", "build"])];
        assert!(best_match("git status && ls && npm run build || echo error", &rules).is_none());
    }

    #[test]
    fn everyday_commands_are_not_signals() {
        let rules = vec![rule(
            "goal-loop",
            "active",
            &["cargo test", "pytest", "tsc"],
        )];
        assert!(best_match("cd core && cargo test --release", &rules).is_none());
        assert!(best_match("python -m pytest -q", &rules).is_none());
    }

    #[test]
    fn a_lone_plain_word_never_matches() {
        let rules = vec![
            rule("coverage", "active", &["coverage"]),
            rule("journal", "pending", &["out of gas", "reverted", "pending", "journal"]),
            rule("recent", "active", &["python3 - <<'EOF'", "grep -n", "AssertionError"]),
        ];
        assert!(best_match("jq .feedback_coverage_7d kpis.json", &rules).is_none());
        assert!(best_match("sqlite3 db \"select state from chunks where state='pending'\"", &rules).is_none());
        assert!(best_match("grep -n best_match core/src/*.rs", &rules).is_none());
        let (r, hits) = best_match("tx reverted, journal row left pending", &rules).unwrap();
        assert_eq!(r.id, "journal");
        assert_eq!(hits, vec!["reverted", "pending", "journal"]);
        let (r, _) = best_match("status: out of gas", &rules).unwrap();
        assert_eq!(r.id, "journal");
    }

    #[test]
    fn mature_rule_wins_a_tie_over_a_candidate() {
        let rules = vec![
            rule("cand", "pending", &["pgrep -f"]),
            rule("mature", "active", &["pgrep -f"]),
        ];
        assert_eq!(best_match("pgrep -f x", &rules).unwrap().0.id, "mature");
    }
}
