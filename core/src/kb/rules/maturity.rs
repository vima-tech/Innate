//! Maturity gates of the rule loop, applied inside curate (replaces the
//! pre-5.0 `repeated_success` / `sustained_usefulness` promotions, which
//! promoted any text an agent had marked "used" twice — status reports
//! included).
//!
//! A candidate becomes mature only when, after it was born:
//! * at least [`MIN_SUPPORTS`] independent sessions reported it *supported*
//!   with an observation (agent verdict, or a judge verdict grounded in a
//!   verbatim quote);
//! * at least one of those sessions was in a project other than the ones the
//!   rule was born from — transferable knowledge, not a project habit;
//! * no counterexample is left unresolved.
//!
//! A mature rule with an unresolved counterexample is suspended back to
//! candidate until the revision step has dealt with it. The user's own rules
//! (created directly as active by `add`) are exempt from both.

use super::*;

pub(crate) const MIN_SUPPORTS: usize = 2;

pub(super) fn json_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_str)
        .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
        .unwrap_or_default()
}

/// A rule the user asked for (`add` → active at creation), not one that must
/// earn its place through validation.
pub(super) fn is_directive(chunk: &Value) -> bool {
    chunk.get("state_reason").and_then(Value::as_str) == Some("init:captured")
        || chunk.get("protected").and_then(Value::as_i64) == Some(1)
}

/// Whether the supports show the rule working outside where it was born.
/// Without a recorded birthplace (pre-5.0 rules), two different known
/// projects among the supports are required instead.
pub(crate) fn transferable(
    supports: &[(String, Option<String>)],
    source_projects: &[String],
) -> bool {
    let known: Vec<&str> = supports.iter().filter_map(|(_, p)| p.as_deref()).collect();
    if source_projects.is_empty() {
        let mut distinct: Vec<&str> = known.clone();
        distinct.sort();
        distinct.dedup();
        distinct.len() >= 2
    } else {
        known
            .iter()
            .any(|p| !source_projects.iter().any(|s| s == p))
    }
}

impl KnowledgeBase {
    pub(crate) fn apply_maturity(&self, now: &str, report: &mut CurateReport) -> Result<()> {
        let mut suspended = 0_i64;
        for id in self.storage.rules_with_open_contradictions()? {
            let Some(chunk) = self.storage.get_chunk(&id)? else {
                continue;
            };
            if chunk.get("state").and_then(Value::as_str) == Some("active") && !is_directive(&chunk)
            {
                self.storage.update_chunk_state(
                    &id,
                    "pending",
                    Some("suspended:contradicted"),
                    now,
                )?;
                suspended += 1;
            }
        }

        let candidates = self.storage.query_chunks(
            "SELECT DISTINCT c.id, c.version, c.source_projects FROM rule_validations v
             JOIN chunks c ON c.id = v.chunk_id
             WHERE v.verdict='supported' AND c.state='pending' AND c.origin != 'spark'",
        )?;
        for c in candidates {
            let id = c.get("id").and_then(Value::as_str).unwrap_or("");
            let version = c.get("version").and_then(Value::as_i64).unwrap_or(1);
            let supports = self.storage.independent_supports(id, version)?;
            if supports.len() < MIN_SUPPORTS
                || !transferable(&supports, &json_list(c.get("source_projects")))
                || !self.storage.open_contradictions(id)?.is_empty()
            {
                continue;
            }
            self.storage
                .update_chunk_state(id, "active", Some("validated:transferable"), now)?;
            report.promoted.push(id.to_string());
        }
        report
            .stats
            .insert("suspended".to_string(), Value::from(suspended));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(p: Option<&str>) -> (String, Option<String>) {
        (crate::utils::gen_uuid(), p.map(str::to_string))
    }

    #[test]
    fn transfer_needs_a_project_outside_the_birthplace() {
        let born = vec!["shop".to_string()];
        assert!(!transferable(&[s(Some("shop")), s(Some("shop"))], &born));
        assert!(transferable(&[s(Some("shop")), s(Some("crm"))], &born));
        assert!(!transferable(&[s(None), s(None)], &born));
    }

    #[test]
    fn unknown_birthplace_needs_two_distinct_projects() {
        assert!(!transferable(&[s(Some("a")), s(Some("a"))], &[]));
        assert!(transferable(&[s(Some("a")), s(Some("b"))], &[]));
    }
}
