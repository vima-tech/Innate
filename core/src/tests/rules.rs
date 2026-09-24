//! The living-knowledge rule loop (schema 5.0): episodes → rule → shown →
//! observed → verdict → mature / suspended / revised.

use super::*;
use crate::kb::RuleVerdict;
use crate::storage::EpisodeRow;

/// A distiller whose model answers from a script: the first reply whose marker
/// appears in the prompt is returned (and consumed).
struct ScriptedModel {
    replies: Mutex<Vec<(&'static str, String)>>,
    calls: AtomicUsize,
}

impl ScriptedModel {
    fn new(replies: Vec<(&'static str, &str)>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(
                replies
                    .into_iter()
                    .map(|(m, r)| (m, r.to_string()))
                    .collect(),
            ),
            calls: AtomicUsize::new(0),
        })
    }
}

impl Distiller for ScriptedModel {
    fn distill(&self, _logs: &[Value]) -> Result<Vec<DistilledChunk>> {
        Ok(vec![])
    }
    fn complete(&self, prompt: &str) -> Option<Result<String>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut replies = self.replies.lock().unwrap();
        let pos = replies.iter().position(|(m, _)| prompt.contains(m));
        Some(match pos {
            Some(p) => Ok(replies.remove(p).1),
            None => Err(InnateError::Other("no scripted reply".into())),
        })
    }
}

const BIRTH: &str = "Episode 1 (";
const JUDGE: &str = "A rule was shown to an agent";
const REVISE: &str = "A rule was contradicted in practice";

fn kb_with(model: Arc<ScriptedModel>) -> (KnowledgeBase, NamedTempFile) {
    let f = NamedTempFile::new().unwrap();
    let kb = KnowledgeBase::open_with(f.path(), None, None, Some(model), None, None).unwrap();
    (kb, f)
}

fn episode(kb: &KnowledgeBase, id: &str, session: &str, project: &str, signal: &str) {
    episode_text(
        kb,
        id,
        session,
        project,
        signal,
        "命令: until ! pgrep -f x; do sleep 2; done",
    );
}

fn episode_text(
    kb: &KnowledgeBase,
    id: &str,
    session: &str,
    project: &str,
    signal: &str,
    cmd: &str,
) {
    kb.storage
        .insert_episode(
            &EpisodeRow {
                id: id.into(),
                kind: "struggle".into(),
                session_id: Some(session.into()),
                project: Some(project.into()),
                ts: crate::utils::utc_now_iso(),
                signals: serde_json::to_string(&[signal]).unwrap(),
                trigger_text: format!("{cmd}\n报错: {signal}"),
                resolution: Some("命令: pgrep -x innate\n结果: ok".into()),
                ..Default::default()
            },
            &crate::utils::utc_now_iso(),
        )
        .unwrap();
}

/// A candidate rule born outside any project the tests validate in.
fn candidate(kb: &KnowledgeBase, content: &str, born_in: &str) -> String {
    let id = kb
        .add(content, "note", Some("t"), None, "manual", None)
        .unwrap();
    kb.storage
        .conn_execute_count(
            "UPDATE chunks SET state='pending', state_reason='init:episodes', protected=0,
                 source_projects=?1, created_at='2026-01-01T00:00:00.000Z'
             WHERE id=?2",
            rusqlite::params![serde_json::json!([born_in]).to_string(), id],
        )
        .unwrap();
    id
}

fn verdict(
    kb: &KnowledgeBase,
    chunk: &str,
    v: &str,
    session: &str,
    project: &str,
) -> RecordReportLite {
    let verdicts = [RuleVerdict {
        chunk_id: chunk.into(),
        verdict: v.into(),
        observation: Some(format!("{v}: observed in {project}")),
    }];
    let report = kb
        .record(RecordParams {
            trace_id: &crate::utils::gen_uuid(),
            verdicts: Some(&verdicts),
            session_id: Some(session),
            project: Some(project),
            source: "sdk",
            ..Default::default()
        })
        .unwrap();
    RecordReportLite(report.rejected_verdicts.len())
}

struct RecordReportLite(usize);

fn state(kb: &KnowledgeBase, id: &str) -> (String, String) {
    let c = kb.storage.get_chunk(id).unwrap().unwrap();
    (
        c["state"].as_str().unwrap().to_string(),
        c["state_reason"].as_str().unwrap_or("").to_string(),
    )
}

#[test]
fn rule_is_born_only_from_episodes_recurring_across_sessions() {
    let model = ScriptedModel::new(vec![(
        BIRTH,
        r#"[{"skill_name":"进程自匹配","content":"用 pgrep -f 等待进程退出时，模式会匹配到调用者自己的命令行……","signals":["pgrep -f","等待循环卡住"],"trigger_desc":"pgrep -f 自匹配"}]"#,
    )]);
    let (kb, _f) = kb_with(Arc::clone(&model));
    episode(&kb, "e1", "s1", "shop", "sig:command timed out");
    episode(&kb, "e2", "s2", "crm", "sig:command timed out");
    episode_text(
        &kb,
        "e3",
        "s3",
        "crm",
        "sig:quota exceeded for mail",
        "给 5000 家企业群发致歉邮件，退信率超过阈值",
    );

    // The dummy test embedder maps every text close to every other; give the
    // episodes orthogonal vectors so only the shared signal can link them.
    let dim = 8;
    for (i, id) in ["e1", "e2", "e3"].iter().enumerate() {
        let mut v = vec![0.0_f32; dim];
        v[i] = 1.0;
        kb.storage
            .set_episode_embedding(id, &crate::utils::pack_embedding(&v))
            .unwrap();
    }

    let report = kb.run_rule_pipeline().unwrap();
    assert_eq!(report["born"], 1, "{report}");

    let rules = kb
        .storage
        .query_chunks("SELECT id, state, state_reason, signals, source_projects FROM chunks")
        .unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0]["state"], "pending");
    assert_eq!(rules[0]["state_reason"], "init:episodes");
    assert!(rules[0]["signals"].as_str().unwrap().contains("pgrep -f"));
    let projects: Vec<String> =
        serde_json::from_str(rules[0]["source_projects"].as_str().unwrap()).unwrap();
    assert_eq!(projects, vec!["shop", "crm"]);
    let open = kb
        .storage
        .query_chunks("SELECT id FROM episodes WHERE state='new'")
        .unwrap();
    assert_eq!(open.len(), 1, "the unrelated episode stays open");
}

#[test]
fn nothing_is_written_without_a_model() {
    let (kb, _f) = tmp_kb();
    episode(&kb, "e1", "s1", "shop", "sig:x failed badly");
    episode(&kb, "e2", "s2", "crm", "sig:x failed badly");
    kb.run_rule_pipeline().unwrap();
    assert!(kb
        .storage
        .query_chunks("SELECT id FROM chunks")
        .unwrap()
        .is_empty());
    let open = kb
        .storage
        .query_chunks("SELECT id FROM episodes WHERE state='new'")
        .unwrap();
    assert_eq!(
        open.len(),
        2,
        "episodes wait for a model instead of becoming copied text"
    );
}

#[test]
fn maturity_needs_independent_support_from_another_project() {
    let (kb, _f) = tmp_kb();
    let rule = candidate(&kb, "rule under test", "shop");

    verdict(&kb, &rule, "supported", "s1", "shop");
    verdict(&kb, &rule, "supported", "s2", "shop");
    kb.builtin_curate_impl(&CurateScope::default()).unwrap();
    assert_eq!(
        state(&kb, &rule).0,
        "pending",
        "two supports inside the birthplace do not transfer"
    );

    // Repeating the same session adds nothing.
    verdict(&kb, &rule, "supported", "s2", "shop");
    verdict(&kb, &rule, "supported", "s3", "crm");
    kb.builtin_curate_impl(&CurateScope::default()).unwrap();
    assert_eq!(
        state(&kb, &rule),
        ("active".into(), "validated:transferable".into())
    );

    verdict(&kb, &rule, "contradicted", "s4", "crm");
    kb.builtin_curate_impl(&CurateScope::default()).unwrap();
    assert_eq!(
        state(&kb, &rule),
        ("pending".into(), "suspended:contradicted".into())
    );
}

#[test]
fn the_users_own_rules_are_not_suspended() {
    let (kb, _f) = tmp_kb();
    let directive = kb
        .add("user directive", "note", Some("t"), None, "manual", None)
        .unwrap();
    verdict(&kb, &directive, "contradicted", "s1", "shop");
    kb.builtin_curate_impl(&CurateScope::default()).unwrap();
    assert_eq!(state(&kb, &directive).0, "active");
}

#[test]
fn verdicts_need_evidence_and_a_real_rule() {
    let (kb, _f) = tmp_kb();
    let rule = candidate(&kb, "rule", "shop");
    let bare = [
        RuleVerdict {
            chunk_id: rule.clone(),
            verdict: "supported".into(),
            observation: None,
        },
        RuleVerdict {
            chunk_id: "no-such-rule".into(),
            verdict: "irrelevant".into(),
            observation: None,
        },
        RuleVerdict {
            chunk_id: rule.clone(),
            verdict: "great".into(),
            observation: Some("x".into()),
        },
        RuleVerdict {
            chunk_id: rule.clone(),
            verdict: "applied".into(),
            observation: None,
        },
    ];
    let report = kb
        .record(RecordParams {
            trace_id: &crate::utils::gen_uuid(),
            verdicts: Some(&bare),
            source: "sdk",
            ..Default::default()
        })
        .unwrap();
    let reasons: Vec<&str> = report
        .rejected_verdicts
        .iter()
        .map(|r| r.reason.as_str())
        .collect();
    assert_eq!(
        reasons,
        vec!["observation_required", "unknown_chunk", "invalid_verdict"]
    );
    let stored = kb
        .storage
        .query_chunks("SELECT verdict FROM rule_validations")
        .unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0]["verdict"], "applied");
    assert_eq!(verdict(&kb, &rule, "supported", "s1", "crm").0, 0);
}

#[test]
fn stop_hook_capture_stores_episodes_and_observations() {
    let (kb, _f) = tmp_kb();
    let rule = candidate(&kb, "pgrep rule", "shop");
    kb.mark_shown(&rule, "pre_tool", Some("s1"), Some("crm"), Some("t1"))
        .unwrap();
    let line = |v: serde_json::Value| v.to_string();
    let transcript = [
        line(serde_json::json!({"type":"assistant","sessionId":"s1","cwd":"/work/crm","timestamp":"2026-09-24T01:00:00.000Z",
            "message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"npm run build"}}]}})),
        line(serde_json::json!({"type":"user","sessionId":"s1","message":{"role":"user",
            "content":[{"type":"tool_result","tool_use_id":"t1","content":"Exit code 2\nerror TS2304: Cannot find name 'foo'","is_error":true}]}})),
        line(serde_json::json!({"type":"assistant","sessionId":"s1","timestamp":"2026-09-24T01:01:00.000Z",
            "message":{"role":"assistant","content":[{"type":"tool_use","id":"t2","name":"Bash","input":{"command":"npm run build"}}]}})),
        line(serde_json::json!({"type":"user","sessionId":"s1","message":{"role":"user",
            "content":[{"type":"tool_result","tool_use_id":"t2","content":"built","is_error":false}]}})),
    ]
    .join("\n");

    let report = kb.capture_session(&transcript).unwrap();
    assert_eq!((report.episodes, report.observations), (1, 1));
    // The Stop hook runs every turn over the whole transcript: nothing doubles.
    let again = kb.capture_session(&transcript).unwrap();
    assert_eq!((again.episodes, again.observations), (0, 0));

    let ep = kb
        .storage
        .query_chunks("SELECT project, session_id, signals FROM episodes")
        .unwrap();
    assert_eq!(ep[0]["project"], "crm");
    assert!(ep[0]["signals"].as_str().unwrap().contains("ts2304"));
    let obs = kb
        .storage
        .query_chunks("SELECT verdict, observation FROM rule_validations")
        .unwrap();
    assert_eq!(obs[0]["verdict"], "observed");
    assert!(obs[0]["observation"]
        .as_str()
        .unwrap()
        .starts_with("[失败] npm run build"));
}

#[test]
fn the_judge_must_quote_what_happened() {
    let model = ScriptedModel::new(vec![
        (
            JUDGE,
            r#"{"applied":true,"verdict":"supported","quote":"built"}"#,
        ),
        (
            JUDGE,
            r#"{"applied":true,"verdict":"supported","quote":"all 40 tests passed"}"#,
        ),
    ]);
    let (kb, _f) = kb_with(model);
    let rule = candidate(&kb, "rule", "shop");
    for (i, obs) in ["[成功] npm run build\nbuilt", "[成功] npm test\nok"]
        .iter()
        .enumerate()
    {
        let session = format!("s{i}");
        kb.mark_shown(&rule, "pre_tool", Some(&session), Some("crm"), Some("t"))
            .unwrap();
        let id = kb.storage.shown_awaiting_observation(&session).unwrap()[0]["id"]
            .as_str()
            .unwrap()
            .to_string();
        kb.storage.set_observation(&id, obs).unwrap();
    }
    kb.run_rule_pipeline().unwrap();
    let mut verdicts: Vec<String> = kb
        .storage
        .query_chunks("SELECT verdict FROM rule_validations ORDER BY created_at")
        .unwrap()
        .iter()
        .map(|r| r["verdict"].as_str().unwrap().to_string())
        .collect();
    verdicts.sort();
    assert_eq!(
        verdicts,
        vec!["supported", "unknown"],
        "an invented quote is not evidence"
    );
}

#[test]
fn a_counterexample_produces_a_new_version() {
    let model = ScriptedModel::new(vec![(
        REVISE,
        r#"{"action":"revise","reason":"pgrep -f also matches the caller","rule":{"content":"用 pgrep -x 或锚定模式……","signals":["pgrep -f","pgrep -x"]}}"#,
    )]);
    let (kb, _f) = kb_with(model);
    let rule = candidate(&kb, "先用 pgrep 找到 PID 再 kill", "shop");
    verdict(&kb, &rule, "contradicted", "s1", "crm");

    let report = kb.run_rule_pipeline().unwrap();
    assert_eq!(report["revised"], 1, "{report}");

    let (old_state, old_reason) = state(&kb, &rule);
    assert_eq!(old_state, "archived");
    let new_id = old_reason
        .strip_prefix("superseded:")
        .expect(&old_reason)
        .to_string();
    let new = kb.storage.get_chunk(&new_id).unwrap().unwrap();
    assert_eq!(new["parent_id"].as_str(), Some(rule.as_str()));
    assert_eq!(new["version"].as_i64(), Some(2));
    assert_eq!(new["state"], "pending");
    assert!(kb.storage.open_contradictions(&rule).unwrap().is_empty());
}

#[test]
fn recall_offers_at_most_one_candidate() {
    let (kb, _f) = tmp_kb();
    let mature = kb
        .add(
            "docker compose healthcheck ordering",
            "note",
            Some("docker compose healthcheck"),
            None,
            "manual",
            None,
        )
        .unwrap();
    for i in 0..3 {
        candidate(
            &kb,
            &format!("docker compose healthcheck variant {i}"),
            "shop",
        );
    }
    let result = kb
        .recall(RecallParams {
            query: "docker compose healthcheck",
            budget: 8000,
            trace: true,
            source: "sdk",
            expand_deps: "false",
            refine_mode: "off",
            ..Default::default()
        })
        .unwrap();
    let states: Vec<&str> = result
        .knowledge
        .iter()
        .map(|c| c["state"].as_str().unwrap_or("?"))
        .collect();
    assert_eq!(
        states.iter().filter(|s| **s == "pending").count(),
        1,
        "{states:?}"
    );
    assert!(result.knowledge.iter().any(|c| c["id"] == mature.as_str()));
}

#[test]
fn inspect_reports_the_rule_loop() {
    let (kb, _f) = tmp_kb();
    let rule = candidate(&kb, "rule", "shop");
    verdict(&kb, &rule, "contradicted", "s1", "crm");
    let report = kb.inspect().unwrap();
    let rules = &report["rules"];
    assert_eq!(rules["candidates"], 1, "{rules}");
    assert_eq!(rules["open_contradictions"], 1);
    assert_eq!(rules["loop_7d"]["validations"]["contradicted"], 1);
}

#[test]
fn a_user_correction_can_seed_a_candidate_on_its_own() {
    let model = ScriptedModel::new(vec![(
        BIRTH,
        r#"[{"skill_name":"首封邮件","content":"首次联系潜在客户的邮件只约沟通时间，不附报价……","signals":["第一封邮件"]}]"#,
    )]);
    let (kb, _f) = kb_with(model);
    kb.storage
        .insert_episode(
            &EpisodeRow {
                id: "c1".into(),
                kind: "correction".into(),
                session_id: Some("s1".into()),
                project: Some("outreach".into()),
                ts: crate::utils::utc_now_iso(),
                signals: "[]".into(),
                trigger_text: "用户纠正: 不对，第一封邮件不要带报价，先约时间".into(),
                ..Default::default()
            },
            &crate::utils::utc_now_iso(),
        )
        .unwrap();
    let report = kb.run_rule_pipeline().unwrap();
    assert_eq!(report["born"], 1, "{report}");
    let (state, reason) = {
        let rows = kb
            .storage
            .query_chunks("SELECT state, state_reason FROM chunks")
            .unwrap();
        (
            rows[0]["state"].as_str().unwrap().to_string(),
            rows[0]["state_reason"].as_str().unwrap().to_string(),
        )
    };
    assert_eq!(
        (state.as_str(), reason.as_str()),
        ("pending", "init:episodes")
    );
}
