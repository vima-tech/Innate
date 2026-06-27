//! Recall-quality evaluation suite.
//!
//! A reproducible IR-style benchmark that grades `recall()` ranking against a
//! labelled fixture (queries → relevant chunk ids) with standard metrics
//! (precision@k, recall@k, MRR, nDCG@k). Its purpose is a **regression safety
//! net for fused-score tuning** — most immediately the ACT-R activation term:
//! the suite proves (1) the pipeline meets a baseline quality bar, (2) turning
//! activation on never *lowers* aggregate quality on the labelled set, and
//! (3) activation actually *raises* ranking when usage history is the only
//! differentiator (recency × frequency).
//!
//! The production `DummyEmbeddingProvider` hashes the *whole* string, so it only
//! yields similarity on exact matches — useless for graded ranking. This suite
//! injects a small **bag-of-words feature-hashing** provider so token overlap
//! produces realistic lexical similarity, the same shape a real embedding model
//! gives. Everything is deterministic: same fixture → same numbers every run.
//!
//! Run with output: `cargo test --release eval -- --nocapture`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::{Duration, SecondsFormat, Utc};
use tempfile::NamedTempFile;

use crate::embedding::EmbeddingProvider;
use crate::errors::Result;
use crate::kb::{KnowledgeBase, RecallParams};
use crate::utils::content_hash;

// ──────────────────────────────────────────────────────────────────────────
// Bag-of-words feature-hashing embedding — token overlap ⇒ cosine similarity.
// ──────────────────────────────────────────────────────────────────────────

struct BowEmbeddingProvider {
    content_dim: usize,
    trigger_dim: usize,
}

impl BowEmbeddingProvider {
    fn new() -> Self {
        Self {
            content_dim: 512,
            trigger_dim: 256,
        }
    }
}

fn bow_vec(text: &str, dim: usize) -> Vec<f32> {
    let mut v = vec![0f32; dim];
    for tok in text.split(|c: char| !c.is_alphanumeric()) {
        if tok.is_empty() {
            continue;
        }
        let h = content_hash(&tok.to_lowercase());
        let idx = usize::from_str_radix(&h[..8], 16).unwrap_or(0) % dim;
        v[idx] += 1.0; // non-negative accumulation → shared tokens add positively
    }
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

impl EmbeddingProvider for BowEmbeddingProvider {
    fn model_name(&self) -> &'static str {
        "BowEmbeddingProvider"
    }
    fn content_dim(&self) -> usize {
        self.content_dim
    }
    fn trigger_dim(&self) -> usize {
        self.trigger_dim
    }
    fn embed_content(&self, text: &str) -> Result<Vec<f32>> {
        Ok(bow_vec(text, self.content_dim))
    }
    fn embed_trigger(&self, text: &str) -> Result<Vec<f32>> {
        Ok(bow_vec(text, self.trigger_dim))
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Metrics
// ──────────────────────────────────────────────────────────────────────────

fn precision_at_k(ranked: &[String], relevant: &HashSet<String>, k: usize) -> f64 {
    if k == 0 {
        return 0.0;
    }
    let hits = ranked
        .iter()
        .take(k)
        .filter(|id| relevant.contains(*id))
        .count();
    hits as f64 / k as f64
}

fn recall_at_k(ranked: &[String], relevant: &HashSet<String>, k: usize) -> f64 {
    if relevant.is_empty() {
        return 0.0;
    }
    let hits = ranked
        .iter()
        .take(k)
        .filter(|id| relevant.contains(*id))
        .count();
    hits as f64 / relevant.len() as f64
}

fn reciprocal_rank(ranked: &[String], relevant: &HashSet<String>) -> f64 {
    for (i, id) in ranked.iter().enumerate() {
        if relevant.contains(id) {
            return 1.0 / (i + 1) as f64;
        }
    }
    0.0
}

fn ndcg_at_k(ranked: &[String], relevant: &HashSet<String>, k: usize) -> f64 {
    let dcg: f64 = ranked
        .iter()
        .take(k)
        .enumerate()
        .map(|(i, id)| {
            if relevant.contains(id) {
                1.0 / ((i + 2) as f64).log2()
            } else {
                0.0
            }
        })
        .sum();
    let ideal_hits = relevant.len().min(k);
    let idcg: f64 = (0..ideal_hits).map(|i| 1.0 / ((i + 2) as f64).log2()).sum();
    if idcg == 0.0 {
        0.0
    } else {
        dcg / idcg
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Fixture
// ──────────────────────────────────────────────────────────────────────────

/// (label, content, trigger) for one knowledge chunk.
const CORPUS: &[(&str, &str, &str)] = &[
    (
        "rust_build",
        "Build the release binary using cargo build release profile",
        "compile rust release binary cargo",
    ),
    (
        "rust_test",
        "Run the rust test suite with the cargo test command",
        "run rust tests cargo suite",
    ),
    (
        "git_branch",
        "Create a new git branch before committing your changes",
        "git branch commit workflow",
    ),
    (
        "git_merge",
        "Resolve git merge conflicts by editing the conflicted files",
        "git merge conflict resolution",
    ),
    (
        "sqlite_tx",
        "Use begin immediate for exclusive write transactions in sqlite",
        "sqlite transaction locking immediate",
    ),
    (
        "sql_index",
        "Add a database index to speed up slow sql queries",
        "sql query performance index optimization",
    ),
    (
        "async_block",
        "Avoid blocking the async runtime with synchronous blocking io",
        "async runtime blocking io",
    ),
    (
        "tokio_spawn",
        "Spawn background tasks using tokio spawn for concurrency",
        "tokio spawn background task concurrency",
    ),
    (
        "unit_test",
        "Write unit tests asserting the expected behavior of functions",
        "unit test assertion behavior",
    ),
    (
        "env_config",
        "Configure environment variables through the settings file",
        "environment variable configuration settings",
    ),
    (
        "api_docs",
        "Document public apis with rust doc comments and examples",
        "api documentation doc comments",
    ),
    (
        "mem_profile",
        "Profile memory usage to find and fix memory leaks",
        "memory profiling leak detection",
    ),
];

/// (query, relevant labels)
const CASES: &[(&str, &[&str])] = &[
    ("cargo build release binary", &["rust_build"]),
    ("run rust test suite cargo", &["rust_test"]),
    ("git merge conflict resolution", &["git_merge"]),
    ("git branch commit workflow", &["git_branch"]),
    ("sqlite transaction locking", &["sqlite_tx"]),
    ("sql query performance index", &["sql_index"]),
    ("async runtime blocking io", &["async_block"]),
    ("tokio spawn background task", &["tokio_spawn"]),
    ("environment variable configuration", &["env_config"]),
    ("memory leak profiling detection", &["mem_profile"]),
];

/// Build a KB with the BoW provider, the optional activation weight applied, and
/// the labelled corpus loaded. Returns the KB, a label→id map, and the temp file
/// (kept alive for the KB's lifetime).
fn build_corpus_kb(
    w_activation: Option<f64>,
) -> (KnowledgeBase, HashMap<String, String>, NamedTempFile) {
    let file = NamedTempFile::new().unwrap();
    // Params are loaded once at open(), so set the weight *before* the handle we
    // actually use: write it on a throwaway handle, drop it, then reopen.
    if let Some(w) = w_activation {
        let seed = KnowledgeBase::open_with(
            file.path(),
            Some(Arc::new(BowEmbeddingProvider::new())),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        seed.storage
            .set_meta("recall.w_activation", &w.to_string())
            .unwrap();
        drop(seed);
    }
    let kb = KnowledgeBase::open_with(
        file.path(),
        Some(Arc::new(BowEmbeddingProvider::new())),
        None,
        None,
        None,
        None,
    )
    .unwrap();

    let mut ids = HashMap::new();
    for (label, content, trigger) in CORPUS {
        let id = kb
            .add(content, "note", Some(trigger), None, "manual", None)
            .unwrap();
        ids.insert((*label).to_string(), id);
    }
    (kb, ids, file)
}

/// ISO timestamp `n` days before now (RFC3339 millis, the format Innate parses).
fn days_ago(n: i64) -> String {
    (Utc::now() - Duration::days(n)).to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[derive(Debug, Default)]
struct Report {
    p_at_1: f64,
    p_at_3: f64,
    recall_at_3: f64,
    mrr: f64,
    ndcg_at_5: f64,
}

/// Run every case and average the metrics.
fn evaluate(kb: &KnowledgeBase, ids: &HashMap<String, String>) -> Report {
    let n = CASES.len() as f64;
    let mut r = Report::default();
    for (query, rel_labels) in CASES {
        let relevant: HashSet<String> = rel_labels
            .iter()
            .map(|l| ids.get(*l).unwrap().clone())
            .collect();
        let ranked = ranked_ids(kb, query, 10);
        r.p_at_1 += precision_at_k(&ranked, &relevant, 1);
        r.p_at_3 += precision_at_k(&ranked, &relevant, 3);
        r.recall_at_3 += recall_at_k(&ranked, &relevant, 3);
        r.mrr += reciprocal_rank(&ranked, &relevant);
        r.ndcg_at_5 += ndcg_at_k(&ranked, &relevant, 5);
    }
    r.p_at_1 /= n;
    r.p_at_3 /= n;
    r.recall_at_3 /= n;
    r.mrr /= n;
    r.ndcg_at_5 /= n;
    r
}

/// Recall ranked chunk ids (fused-score order) for a query.
fn ranked_ids(kb: &KnowledgeBase, query: &str, top: usize) -> Vec<String> {
    let res = kb
        .recall(RecallParams {
            query,
            budget: 1_000_000, // large ⇒ no packing drops ⇒ pure fused-order ranking
            trace: false,
            include_sparks: false,
            top: Some(top),
            source: "sdk",
            expand_deps: "false",
            allow_trim: false,
            refine_mode: "off",
            min_score: None,
            session_only: false,
            ..Default::default()
        })
        .unwrap();
    res.knowledge
        .iter()
        .filter_map(|c| c["id"].as_str().map(str::to_string))
        .collect()
}

// ──────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn baseline_recall_quality_meets_thresholds() {
    // Whole-pipeline regression guard: with realistic lexical embeddings the
    // labelled fixture must clear a quality bar. If a future change tanks recall
    // ranking, these thresholds fail loudly.
    let (kb, ids, _f) = build_corpus_kb(None);
    let r = evaluate(&kb, &ids);
    eprintln!(
        "[eval] baseline  P@1={:.3} P@3={:.3} R@3={:.3} MRR={:.3} nDCG@5={:.3}",
        r.p_at_1, r.p_at_3, r.recall_at_3, r.mrr, r.ndcg_at_5
    );
    assert!(r.p_at_1 >= 0.8, "P@1 {:.3} below 0.8", r.p_at_1);
    assert!(r.mrr >= 0.85, "MRR {:.3} below 0.85", r.mrr);
    assert!(r.recall_at_3 >= 0.9, "R@3 {:.3} below 0.9", r.recall_at_3);
    assert!(r.ndcg_at_5 >= 0.85, "nDCG@5 {:.3} below 0.85", r.ndcg_at_5);
}

#[test]
fn actr_activation_does_not_regress_quality() {
    // The ACT-R safety net: sprinkle realistic usage history over the corpus
    // (some relevant, some on distractors), then confirm enabling activation
    // never *lowers* aggregate ranking quality versus activation off. This is the
    // guard rail for tuning recall.w_activation upward.
    let seed_usage = |kb: &KnowledgeBase, ids: &HashMap<String, String>| {
        // Adversarial usage history: pile heavy, recent usage onto *distractors*
        // that share query tokens with a relevant chunk (unit_test↔rust_test,
        // git_branch↔git_merge, async_block↔tokio_spawn), plus some on relevant
        // ones. The default activation weight must not let usage override lexical
        // relevance — if a future bump to recall.w_activation does, this fails.
        let recent = [
            ("unit_test", 40),
            ("git_branch", 40),
            ("async_block", 35),
            ("api_docs", 30),
            ("rust_build", 25),
            ("sql_index", 8),
        ];
        for (label, count) in recent {
            kb.storage
                .conn_execute(
                    "UPDATE chunks SET used_count=?, used_count_base=?, last_used_at=? WHERE id=?",
                    rusqlite::params![count, count, days_ago(1), ids.get(label).unwrap()],
                )
                .unwrap();
        }
    };

    let (kb_off, ids_off, _f1) = build_corpus_kb(Some(0.0));
    seed_usage(&kb_off, &ids_off);
    let off = evaluate(&kb_off, &ids_off);

    let (kb_on, ids_on, _f2) = build_corpus_kb(Some(0.08));
    seed_usage(&kb_on, &ids_on);
    let on = evaluate(&kb_on, &ids_on);

    eprintln!(
        "[eval] act-off   P@1={:.3} MRR={:.3} nDCG@5={:.3}",
        off.p_at_1, off.mrr, off.ndcg_at_5
    );
    eprintln!(
        "[eval] act-on    P@1={:.3} MRR={:.3} nDCG@5={:.3}",
        on.p_at_1, on.mrr, on.ndcg_at_5
    );
    // No regression (allow a hair of float slack).
    assert!(
        on.mrr >= off.mrr - 1e-9,
        "activation regressed MRR: {:.4} < {:.4}",
        on.mrr,
        off.mrr
    );
    assert!(
        on.ndcg_at_5 >= off.ndcg_at_5 - 1e-9,
        "activation regressed nDCG: {:.4} < {:.4}",
        on.ndcg_at_5,
        off.ndcg_at_5
    );
    assert!(
        on.p_at_1 >= off.p_at_1 - 1e-9,
        "activation regressed P@1: {:.4} < {:.4}",
        on.p_at_1,
        off.p_at_1
    );
}

#[test]
fn actr_activation_overweighting_visibly_regresses() {
    // Sensitivity check — proves the harness genuinely measures activation's
    // effect (so the no-regression test isn't trivially always-green). An absurd
    // weight lets heavy distractor usage override lexical relevance, which the
    // metrics must catch. This documents that w_activation lives in a safe band:
    // small (0.08) holds quality, huge (5.0) breaks it.
    let seed_distractors = |kb: &KnowledgeBase, ids: &HashMap<String, String>| {
        for (label, count) in [("unit_test", 50), ("git_branch", 50), ("async_block", 50)] {
            kb.storage
                .conn_execute(
                    "UPDATE chunks SET used_count=?, used_count_base=?, last_used_at=? WHERE id=?",
                    rusqlite::params![count, count, days_ago(0), ids.get(label).unwrap()],
                )
                .unwrap();
        }
    };

    let (kb, ids, _f) = build_corpus_kb(Some(5.0));
    seed_distractors(&kb, &ids);
    let r = evaluate(&kb, &ids);
    eprintln!(
        "[eval] overweight(5.0) P@1={:.3} MRR={:.3} nDCG@5={:.3}",
        r.p_at_1, r.mrr, r.ndcg_at_5
    );
    assert!(
        r.mrr < 1.0,
        "an absurd activation weight should override relevance and drop MRR below 1.0, got {:.3}",
        r.mrr
    );
}

#[test]
fn actr_activation_breaks_ties_by_recency_and_frequency() {
    // Value demonstration: two chunks equally relevant to a query on embeddings
    // (identical trigger, content differing only by a token absent from the
    // query) — so the *only* discriminator is usage history. Activation must
    // float the frequently-and-recently-used one above the cold one.
    let file = NamedTempFile::new().unwrap();
    let kb = KnowledgeBase::open_with(
        file.path(),
        Some(Arc::new(BowEmbeddingProvider::new())),
        None,
        None,
        None,
        None,
    )
    .unwrap();

    let trigger = "deploy service rollout procedure";
    let hot = kb
        .add(
            "deploy service rollout procedure variant alpha",
            "note",
            Some(trigger),
            None,
            "manual",
            None,
        )
        .unwrap();
    let cold = kb
        .add(
            "deploy service rollout procedure variant beta",
            "note",
            Some(trigger),
            None,
            "manual",
            None,
        )
        .unwrap();

    // hot: used 30× and just now; cold: never used.
    kb.storage
        .conn_execute(
            "UPDATE chunks SET used_count=30, used_count_base=30, last_used_at=? WHERE id=?",
            rusqlite::params![days_ago(0), hot],
        )
        .unwrap();

    let ranked = ranked_ids(&kb, "deploy service rollout procedure", 5);
    let pos = |id: &str| ranked.iter().position(|x| x == id);
    let (hp, cp) = (pos(&hot), pos(&cold));
    eprintln!("[eval] tie-break ranked={ranked:?} hot@{hp:?} cold@{cp:?}");
    assert!(
        hp.is_some() && cp.is_some(),
        "both variants should be retrieved"
    );
    assert!(
        hp.unwrap() < cp.unwrap(),
        "activation should rank the hot (used 30×, recent) variant above the cold one"
    );
}

#[test]
fn spreading_activation_links_chunks_through_shared_entity() {
    // ACT-R spreading activation (SAG-inspired associative recall). Chunk B shares
    // NO query token (zero similarity/lexical signal) but shares a discriminative
    // code symbol with chunk A, which the query *does* hit. With w_spread > 0, the
    // activation spreads A→entity→B, giving B a positive spread score it cannot
    // get on the default (w_spread = 0) path.
    let file = NamedTempFile::new().unwrap();
    // Seed the weight, then reopen so load_params picks it up (open_with loads once).
    {
        let seed = KnowledgeBase::open_with(
            file.path(),
            Some(Arc::new(BowEmbeddingProvider::new())),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        seed.storage.set_meta("recall.w_spread", "0.6").unwrap();
    }
    let kb = KnowledgeBase::open_with(
        file.path(),
        Some(Arc::new(BowEmbeddingProvider::new())),
        None,
        None,
        None,
        None,
    )
    .unwrap();

    // A is hit by the query trigger and carries the discriminative symbol.
    let _a = kb
        .add(
            "tune the fused score via Wug_Plover::quux knob",
            "note",
            Some("embedding cache invalidation strategy"),
            None,
            "manual",
            None,
        )
        .unwrap();
    // B shares the symbol but nothing else with the query.
    let id_b = kb
        .add(
            "Wug_Plover::quux also guards the zebra xylophone edge case",
            "note",
            Some("unrelated marsupial topic"),
            None,
            "manual",
            None,
        )
        .unwrap();
    // Noise so the corpus isn't trivially two rows.
    for i in 0..4 {
        kb.add(
            &format!("totally unrelated note number {i} about gardening"),
            "note",
            Some(&format!("gardening tip {i}")),
            None,
            "manual",
            None,
        )
        .unwrap();
    }

    let spread_of = |id: &str| -> f64 {
        let res = kb
            .recall(RecallParams {
                query: "embedding cache invalidation strategy",
                budget: 1_000_000,
                source: "sdk",
                ..Default::default()
            })
            .unwrap();
        res.knowledge
            .iter()
            .find(|c| c["id"].as_str() == Some(id))
            .and_then(|c| c["_sim_spread"].as_f64())
            .unwrap_or(0.0)
    };

    let b_spread = spread_of(&id_b);
    eprintln!("[eval] B spread score = {b_spread}");
    assert!(
        b_spread > 0.0,
        "B should receive positive spreading activation via the shared Wug_Plover::quux entity"
    );
}
