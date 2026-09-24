//! Offline steps of the rule loop, run by `evolve` before each curate:
//! judge observations, write rules from recurring episodes, revise
//! contradicted rules, and give pre-5.0 rules their signals.
//!
//! Every step is capped per run: the configured model answers in ~50 s at the
//! median, and a run must finish well inside the evolve lease. Work left over
//! waits for the next run. When the distiller has no model behind it,
//! `complete` returns `None` and the pipeline stops — it never falls back to a
//! deterministic writer, because a copied summary is not a rule.

use std::collections::{HashMap, HashSet};

use super::*;
use crate::rule_prompts::{self, Judgement, Revision, RuleDraft};
use crate::utils::{cosine_similarity, unpack_embedding};

const JUDGE_PER_RUN: i64 = 4;
const BIRTH_PER_RUN: usize = 2;
const REVISE_PER_RUN: usize = 1;
const BACKFILL_PER_RUN: i64 = 3;
const EMBED_EPISODES_PER_RUN: usize = 12;
/// Episodes considered for clustering in one run.
const EPISODE_POOL: i64 = 400;
/// A signal shared by more episodes than this is too common to mean anything.
const SIGNAL_FAN_MAX: usize = 8;
/// Two episodes this similar describe the same situation. Model-dependent:
/// with the configured embedding model (2026-09-24, 24 real episodes) cross-
/// session pairs sat at p50 0.47 / p99 0.69 / max 0.72, and pairs at ~0.7
/// shared only surface wording ("sleep N; cd …"). Kept strict on purpose —
/// a spurious cluster costs a ~50 s model call for nothing — so in practice
/// clustering is signal-driven, and user corrections enter as singletons.
const EPISODE_COSINE_MIN: f32 = 0.82;
const CLUSTER_MAX: usize = 6;
/// Episodes that never recurred within this window stop being candidates.
const EPISODE_WINDOW_DAYS: i64 = 60;
/// A shown rule whose observation never arrived becomes `unknown`.
const SHOWN_TTL_DAYS: i64 = 2;

enum Model<T> {
    Answer(T),
    /// No model behind the distiller, or the call failed: stop this step.
    Unavailable,
}

impl KnowledgeBase {
    fn ask(&self, prompt: &str) -> Model<String> {
        match self.distiller.complete(prompt) {
            Some(Ok(text)) => Model::Answer(text),
            Some(Err(e)) => {
                eprintln!("[innate] rule pipeline: model call failed: {e}");
                Model::Unavailable
            }
            None => Model::Unavailable,
        }
    }

    /// Run the rule pipeline, then curate. The pipeline's report rides along in
    /// `CurateReport.stats["rules"]`; a pipeline error never blocks curate.
    pub(crate) fn curate_with_rules(&self) -> Result<CurateReport> {
        let rules = self
            .run_rule_pipeline()
            .unwrap_or_else(|e| json!({ "error": e.to_string() }));
        let curator = Arc::clone(&self.curator);
        let mut curate = curator.run(self, &CurateScope::default())?;
        curate.stats.insert("rules".to_string(), rules);
        Ok(curate)
    }

    pub(crate) fn run_rule_pipeline(&self) -> Result<Value> {
        let now = utc_now_iso();
        let expired_shown = self
            .storage
            .expire_unobserved_before(&days_ago(&now, SHOWN_TTL_DAYS))?;
        let expired_episodes = self
            .storage
            .expire_episodes_before(&days_ago(&now, EPISODE_WINDOW_DAYS))?;
        let judged = self.judge_observations()?;
        let born = self.birth_rules()?;
        let revised = self.revise_contradicted()?;
        let backfilled = self.backfill_signals()?;
        Ok(json!({
            "judged": judged,
            "born": born,
            "revised": revised,
            "signals_backfilled": backfilled,
            "expired_shown": expired_shown,
            "expired_episodes": expired_episodes,
        }))
    }

    fn judge_observations(&self) -> Result<usize> {
        let mut n = 0;
        for row in self.storage.observations_to_judge(JUDGE_PER_RUN)? {
            let id = row.get("id").and_then(Value::as_str).unwrap_or("");
            let observation = row.get("observation").and_then(Value::as_str).unwrap_or("");
            let rule = row.get("content").and_then(Value::as_str).unwrap_or("");
            let Model::Answer(raw) = self.ask(&rule_prompts::judge(rule, observation)) else {
                break;
            };
            let Judgement { verdict, .. } = rule_prompts::parse_judgement(&raw, observation)
                .unwrap_or(Judgement {
                    verdict: "unknown",
                    quote: None,
                });
            self.storage
                .set_judged_verdict(id, verdict, &utc_now_iso())?;
            n += 1;
        }
        Ok(n)
    }

    /// Embed episodes that have no vector yet, so meaning-based clustering can
    /// see them. Failures are skipped: signal clustering still works.
    fn embed_episodes(&self, episodes: &[Value]) {
        let pending = episodes
            .iter()
            .filter(|e| e.get("has_embedding").and_then(Value::as_i64) == Some(0))
            .take(EMBED_EPISODES_PER_RUN);
        for e in pending {
            let text = format!(
                "{}\n{}",
                e.get("trigger_text").and_then(Value::as_str).unwrap_or(""),
                e.get("resolution").and_then(Value::as_str).unwrap_or("")
            );
            let id = e.get("id").and_then(Value::as_str).unwrap_or("");
            match self.embedding.embed_content(&text) {
                Ok(v) if v.len() == self.embedding.content_dim() => {
                    let _ = self.storage.set_episode_embedding(id, &pack_embedding(&v));
                }
                _ => {}
            }
        }
    }

    fn birth_rules(&self) -> Result<usize> {
        let episodes = self.storage.open_episodes(EPISODE_POOL)?;
        if episodes.is_empty() {
            return Ok(0);
        }
        self.embed_episodes(&episodes);
        let vectors: Vec<Option<Vec<f32>>> = episodes
            .iter()
            .map(|e| {
                let id = e.get("id").and_then(Value::as_str).unwrap_or("");
                self.storage
                    .episode_embedding(id)
                    .ok()
                    .flatten()
                    .map(|b| unpack_embedding(&b))
            })
            .collect();
        let mut born = 0;
        // A user correction is the user saying "this was wrong" — as explicit
        // as a nomination — and carries no technical signal to cluster on, so
        // it may seed a candidate on its own. Recurring clusters go first.
        let clusters = cluster_episodes(&episodes, &vectors);
        let clustered: std::collections::HashSet<usize> =
            clusters.iter().flatten().copied().collect();
        let corrections = (0..episodes.len())
            .filter(|i| !clustered.contains(i))
            .filter(|&i| episodes[i].get("kind").and_then(Value::as_str) == Some("correction"))
            .map(|i| vec![i]);
        for cluster in clusters.into_iter().chain(corrections).take(BIRTH_PER_RUN) {
            let members: Vec<Value> = cluster.iter().map(|&i| episodes[i].clone()).collect();
            let ids: Vec<String> = members
                .iter()
                .filter_map(|e| e.get("id").and_then(Value::as_str).map(str::to_string))
                .collect();
            let Model::Answer(raw) = self.ask(&rule_prompts::rule_from_episodes(&members)) else {
                break;
            };
            let drafts = match rule_prompts::parse_rules(&raw) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("[innate] rule pipeline: unparsable rule reply: {e}");
                    continue; // episodes stay open; the next run retries
                }
            };
            let projects: Vec<String> = distinct(
                members
                    .iter()
                    .filter_map(|e| e.get("project").and_then(Value::as_str)),
            );
            let agent = members
                .iter()
                .find_map(|e| e.get("agent").and_then(Value::as_str))
                .map(str::to_string);
            let mut first_rule: Option<String> = None;
            for draft in drafts {
                let parent = ParentRef::default();
                if let Some(id) =
                    self.insert_rule(&draft, &projects, agent.clone(), "init:episodes", &parent)?
                {
                    first_rule.get_or_insert(id);
                    born += 1;
                }
            }
            match first_rule {
                Some(rule_id) => self.storage.mark_episodes(&ids, "ruled", Some(&rule_id))?,
                None => self.storage.mark_episodes(&ids, "no_rule", None)?,
            }
        }
        Ok(born)
    }

    /// Write one rule as a pending chunk with its vectors. Returns `None` when
    /// the text is discarded by the sanitizer, was invalidated before, or could
    /// not be embedded.
    fn insert_rule(
        &self,
        draft: &RuleDraft,
        source_projects: &[String],
        agent: Option<String>,
        reason: &str,
        parent: &ParentRef,
    ) -> Result<Option<String>> {
        let (content, action) = self.sanitize_content(&draft.content);
        if action == SanitizeAction::Discard {
            return Ok(None);
        }
        let hash = content_hash(&content);
        if self.storage.is_hash_invalidated(&hash)? {
            return Ok(None);
        }
        let trigger = draft
            .trigger_desc
            .clone()
            .or_else(|| (!draft.signals.is_empty()).then(|| draft.signals.join(" | ")));
        let (cvec, tvec) =
            self.embed_pair(&content, trigger.as_deref().unwrap_or(&content), "rule");
        let (Ok(cvec), Ok(tvec)) = (cvec, tvec) else {
            return Ok(None);
        };
        if cvec.len() != self.embedding.content_dim() || tvec.len() != self.embedding.trigger_dim()
        {
            return Ok(None);
        }
        let now = utc_now_iso();
        let provenance = self.distiller.provenance();
        let id = gen_uuid();
        let row = ChunkRow {
            id: id.clone(),
            skill_name: draft.skill_name.clone().or_else(|| trigger.clone()),
            content: content.clone(),
            trigger_desc: trigger,
            anti_trigger_desc: draft.anti_trigger_desc.clone(),
            content_hash: hash,
            token_count: Some(estimate_tokens(&content) as i64),
            origin: "distilled".to_string(),
            agent,
            distill_provider: provenance.provider,
            distill_model: provenance.model,
            distill_prompt_version: Some(rule_prompts::PROMPT_VERSION.to_string()),
            parent_id: parent.id.clone(),
            version: parent.version + 1,
            signals: Some(serde_json::to_string(&draft.signals).unwrap_or_else(|_| "[]".into())),
            source_projects: Some(
                serde_json::to_string(source_projects).unwrap_or_else(|_| "[]".into()),
            ),
            state: "pending".to_string(),
            state_reason: Some(reason.to_string()),
            confidence: DISTILLED_SEED_CONFIDENCE,
            confidence_reason: Some(reason.to_string()),
            embed_version: 1,
            created_at: now.clone(),
            updated_at: now,
            ..Default::default()
        };
        self.storage.begin_immediate()?;
        let write = (|| -> Result<()> {
            self.storage.insert_chunk(&row)?;
            self.storage
                .insert_vec_content(&id, &pack_embedding(&cvec))?;
            self.storage
                .insert_vec_trigger(&id, &pack_embedding(&tvec))?;
            self.storage.commit()
        })();
        if let Err(e) = write {
            let _ = self.storage.rollback();
            return Err(e);
        }
        Ok(Some(id))
    }

    fn revise_contradicted(&self) -> Result<usize> {
        let mut n = 0;
        for chunk_id in self
            .storage
            .rules_with_open_contradictions()?
            .into_iter()
            .take(REVISE_PER_RUN)
        {
            let Some(chunk) = self.storage.get_chunk(&chunk_id)? else {
                continue;
            };
            if maturity::is_directive(&chunk) {
                continue; // the user's own rules are theirs to change
            }
            let contradictions = self.storage.open_contradictions(&chunk_id)?;
            let examples: Vec<String> = contradictions
                .iter()
                .filter_map(|c| {
                    c.get("observation")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect();
            let content = chunk.get("content").and_then(Value::as_str).unwrap_or("");
            let signals = chunk.get("signals").and_then(Value::as_str).unwrap_or("[]");
            let Model::Answer(raw) = self.ask(&rule_prompts::revise(content, signals, &examples))
            else {
                break;
            };
            let now = utc_now_iso();
            match rule_prompts::parse_revision(&raw) {
                Ok(Revision::Revise(draft, _reason)) => {
                    let mut projects = maturity::json_list(chunk.get("source_projects"));
                    for c in &contradictions {
                        if let Some(p) = c.get("project").and_then(Value::as_str) {
                            if !projects.iter().any(|x| x == p) {
                                projects.push(p.to_string());
                            }
                        }
                    }
                    let parent = ParentRef {
                        id: Some(chunk_id.clone()),
                        version: chunk.get("version").and_then(Value::as_i64).unwrap_or(1),
                    };
                    let agent = chunk
                        .get("agent")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    let reason = format!("revision_of:{chunk_id}");
                    if let Some(new_id) =
                        self.insert_rule(&draft, &projects, agent, &reason, &parent)?
                    {
                        self.storage.update_chunk_state(
                            &chunk_id,
                            "archived",
                            Some(&format!("superseded:{new_id}")),
                            &now,
                        )?;
                    }
                }
                Ok(Revision::Retire(_)) => {
                    self.storage.update_chunk_state(
                        &chunk_id,
                        "archived",
                        Some("retired:counterexample"),
                        &now,
                    )?;
                }
                Ok(Revision::Keep(_)) => {}
                Err(e) => {
                    eprintln!("[innate] rule pipeline: unparsable revision reply: {e}");
                    continue;
                }
            }
            self.storage.resolve_contradictions(&chunk_id, &now)?;
            n += 1;
        }
        Ok(n)
    }

    fn backfill_signals(&self) -> Result<usize> {
        let mut n = 0;
        for row in self.storage.rules_missing_signals(BACKFILL_PER_RUN)? {
            let id = row.get("id").and_then(Value::as_str).unwrap_or("");
            let text = format!(
                "{}\n{}",
                row.get("content").and_then(Value::as_str).unwrap_or(""),
                row.get("trigger_desc")
                    .and_then(Value::as_str)
                    .unwrap_or("")
            );
            let Model::Answer(raw) = self.ask(&rule_prompts::signals_for(&text)) else {
                break;
            };
            // Keep only signals grounded in the rule's own text: asked to list
            // signals for a vague rule, the model invents plausible tools
            // (live run: "BLEU", "promtool", "pytest" for rules naming none of
            // them), and an invented common command would inject the rule on
            // every matching shell call. An empty list is stored too, so the
            // rule is not asked about again.
            let lower = text.to_lowercase();
            let signals: Vec<String> = rule_prompts::parse_signals(&raw)
                .unwrap_or_default()
                .into_iter()
                .filter(|s| lower.contains(&s.to_lowercase()))
                .collect();
            self.storage.set_chunk_signals(
                id,
                &serde_json::to_string(&signals).unwrap_or_else(|_| "[]".into()),
            )?;
            n += 1;
        }
        Ok(n)
    }
}

/// The rule a new version replaces (`None` for a newborn rule).
#[derive(Default)]
struct ParentRef {
    id: Option<String>,
    version: i64,
}

fn distinct<'a>(items: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in items {
        if !s.is_empty() && !out.iter().any(|x| x == s) {
            out.push(s.to_string());
        }
    }
    out
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    cosine_similarity(a, b)
}

/// Group episodes that recur across sessions. Two episodes from different
/// sessions are linked when they share a discriminative signal or are close in
/// meaning; a group becomes a candidate only when it spans ≥2 sessions.
/// Deterministic for a given input, largest groups first.
pub(super) fn cluster_episodes(
    episodes: &[Value],
    vectors: &[Option<Vec<f32>>],
) -> Vec<Vec<usize>> {
    let n = episodes.len();
    let session = |i: usize| {
        episodes[i]
            .get("session_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut [usize], x: usize) -> usize {
        let mut r = x;
        while p[r] != r {
            r = p[r];
        }
        let mut y = x;
        while p[y] != r {
            let next = p[y];
            p[y] = r;
            y = next;
        }
        r
    }
    let union = |p: &mut Vec<usize>, a: usize, b: usize| {
        let (ra, rb) = (find(p, a), find(p, b));
        if ra != rb {
            p[ra.max(rb)] = ra.min(rb);
        }
    };

    let mut by_signal: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, e) in episodes.iter().enumerate() {
        let signals: Vec<String> = e
            .get("signals")
            .and_then(Value::as_str)
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        for s in signals {
            by_signal.entry(s).or_default().push(i);
        }
    }
    let mut keys: Vec<&String> = by_signal.keys().collect();
    keys.sort();
    for k in keys {
        let members = &by_signal[k];
        if members.len() < 2 || members.len() > SIGNAL_FAN_MAX {
            continue;
        }
        let sessions: HashSet<String> = members.iter().map(|&m| session(m)).collect();
        if sessions.len() >= 2 {
            for &m in &members[1..] {
                union(&mut parent, members[0], m);
            }
        }
    }
    for i in 0..n {
        let Some(a) = vectors.get(i).and_then(Option::as_ref) else {
            continue;
        };
        for j in i + 1..n {
            let Some(b) = vectors.get(j).and_then(Option::as_ref) else {
                continue;
            };
            if session(i) != session(j) && cosine(a, b) >= EPISODE_COSINE_MIN {
                union(&mut parent, i, j);
            }
        }
    }

    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..n {
        let r = find(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }
    let mut out: Vec<Vec<usize>> = groups
        .into_values()
        .filter(|g| g.iter().map(|&i| session(i)).collect::<HashSet<_>>().len() >= 2)
        .map(|mut g| {
            g.sort();
            g.truncate(CLUSTER_MAX);
            g
        })
        .collect();
    out.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(session: &str, signals: &[&str]) -> Value {
        json!({"session_id": session, "signals": serde_json::to_string(signals).unwrap()})
    }

    #[test]
    fn shared_signal_across_sessions_forms_a_cluster() {
        let eps = vec![
            ep("s1", &["sig:error: unique constraint failed"]),
            ep("s2", &["sig:error: unique constraint failed", "e0425"]),
            ep("s3", &["unrelated"]),
        ];
        assert_eq!(
            cluster_episodes(&eps, &[None, None, None]),
            vec![vec![0, 1]]
        );
    }

    #[test]
    fn same_session_repetition_is_not_recurrence() {
        let eps = vec![ep("s1", &["sig:x failed"]), ep("s1", &["sig:x failed"])];
        assert!(cluster_episodes(&eps, &[None, None]).is_empty());
    }

    #[test]
    fn meaning_links_signal_free_corrections() {
        let eps = vec![ep("s1", &[]), ep("s2", &[])];
        let v = Some(vec![1.0_f32, 0.0, 0.0]);
        let w = Some(vec![0.95_f32, 0.05, 0.0]);
        assert_eq!(cluster_episodes(&eps, &[v, w]), vec![vec![0, 1]]);
    }

    #[test]
    fn promiscuous_signals_do_not_link() {
        let eps: Vec<Value> = (0..10)
            .map(|i| ep(&format!("s{i}"), &["--release"]))
            .collect();
        assert!(cluster_episodes(&eps, &vec![None; 10]).is_empty());
    }
}
