use super::*;

mod evidence;

impl KnowledgeBase {
    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &self,
        trace_id: &str,
        query: Option<&str>,
        output: Option<&str>,
        output_summary: Option<&str>,
        outcome: Option<&str>,
        used: Option<&[String]>,
        feedback_up: Option<&[String]>,
        feedback_down: Option<&[String]>,
        nomination: Option<&str>,
        priority: i64,
        source: &str,
    ) -> Result<()> {
        self.record_detailed(
            trace_id,
            query,
            output,
            output_summary,
            outcome,
            used,
            "explicit",
            true,
            feedback_up,
            feedback_down,
            "user",
            None,
            None,
            nomination,
            priority,
            None,
            source,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_detailed(
        &self,
        trace_id: &str,
        query: Option<&str>,
        output: Option<&str>,
        output_summary: Option<&str>,
        outcome: Option<&str>,
        used: Option<&[String]>,
        used_attribution: &str,
        used_complete: bool,
        feedback_up: Option<&[String]>,
        feedback_down: Option<&[String]>,
        feedback_kind: &str,
        feedback_actor: Option<&str>,
        feedback_reason: Option<&str>,
        nomination: Option<&str>,
        priority: i64,
        task_state: Option<&str>,
        source: &str,
    ) -> Result<()> {
        let dedupe_ids = |ids: &[String]| {
            let mut seen = HashSet::new();
            ids.iter()
                .filter(|id| seen.insert((*id).clone()))
                .cloned()
                .collect::<Vec<_>>()
        };
        let normalized_used = used.map(dedupe_ids);
        let normalized_feedback_up = feedback_up.map(dedupe_ids);
        let normalized_feedback_down = feedback_down.map(dedupe_ids);
        let used = normalized_used.as_deref();
        let feedback_up = normalized_feedback_up.as_deref();
        let feedback_down = normalized_feedback_down.as_deref();

        if let Some(o) = outcome {
            if !matches!(o, "ok" | "fail" | "unknown") {
                return Err(InnateError::InvalidState(format!("invalid outcome: {o}")));
            }
        }
        if !matches!(used_attribution, "explicit" | "cited" | "inferred") {
            return Err(InnateError::InvalidState(format!(
                "invalid used attribution: {used_attribution}"
            )));
        }
        if !matches!(feedback_kind, "user" | "judge") {
            return Err(InnateError::InvalidState(format!(
                "invalid feedback kind: {feedback_kind}"
            )));
        }
        if let Some(state) = task_state {
            if !matches!(
                state,
                "recalled" | "running" | "completed" | "abandoned" | "timed_out"
            ) {
                return Err(InnateError::InvalidState(format!(
                    "invalid task state: {state}"
                )));
            }
        }
        validate_source(source)?;
        if let (Some(ups), Some(downs)) = (feedback_up, feedback_down) {
            let down_set: HashSet<&str> = downs.iter().map(String::as_str).collect();
            if let Some(chunk_id) = ups.iter().find(|id| down_set.contains(id.as_str())) {
                return Err(InnateError::InvalidState(format!(
                    "conflicting feedback for chunk {chunk_id}"
                )));
            }
        }
        let effective_priority = if nomination.is_some() && priority == 0 {
            1
        } else {
            priority
        };
        let now = utc_now_iso();
        let lib_id = self.storage.lib_id()?;

        self.storage.begin_immediate()?;
        let result = (|| -> Result<()> {
            let log = self.storage.get_episodic_log(trace_id)?;
            let mut is_fresh_insert = false;
            let log = match log {
                Some(l) => l,
                None => {
                    let used_ids = used.map(serde_json::to_string).transpose()?;
                    let row = EpisodicLogRow {
                        id: gen_uuid(),
                        trace_id: trace_id.to_string(),
                        lib_id,
                        ts: now.clone(),
                        query: query.map(str::to_string).or_else(|| Some(String::new())),
                        output: output.map(str::to_string),
                        output_summary: output_summary.map(str::to_string),
                        outcome: outcome.map(str::to_string),
                        event_source: source.to_string(),
                        task_state: if matches!(outcome, Some("ok") | Some("fail")) {
                            "completed".to_string()
                        } else {
                            task_state.unwrap_or("running").to_string()
                        },
                        completed_at: matches!(outcome, Some("ok") | Some("fail"))
                            .then(|| now.clone()),
                        usage_state: usage_state(used).to_string(),
                        used_ids,
                        used_attribution: used.map(|_| used_attribution.to_string()),
                        used_complete,
                        context_key: query.map(|q| content_hash(&normalize_query(q))),
                        nomination: nomination.map(str::to_string),
                        priority: effective_priority,
                        distill_state: "open".to_string(),
                        ..Default::default()
                    };
                    self.storage.upsert_episodic_log(&row)?;
                    is_fresh_insert = true;
                    self.storage.get_episodic_log(trace_id)?.unwrap()
                }
            };
            self.validate_trace_attribution(trace_id, used, "used")?;
            self.validate_trace_attribution(trace_id, feedback_up, "feedback_up")?;
            self.validate_trace_attribution(trace_id, feedback_down, "feedback_down")?;

            let existing_outcome = log
                .get("outcome")
                .and_then(Value::as_str)
                .map(str::to_string);
            // usage_trace: used
            let effective_used_attribution = if used.is_some() {
                used_attribution
            } else {
                log.get("used_attribution")
                    .and_then(Value::as_str)
                    .unwrap_or(used_attribution)
            };
            let used_strength = match effective_used_attribution {
                "explicit" => 0.3,
                "cited" => 0.25,
                "inferred" => 0.15,
                _ => unreachable!(),
            };
            let existing_used_ids: Vec<String> = log
                .get("used_ids")
                .and_then(Value::as_str)
                .and_then(|raw| serde_json::from_str(raw).ok())
                .unwrap_or_default();
            let existing_used_complete = log
                .get("used_complete")
                .and_then(Value::as_i64)
                .unwrap_or(0)
                != 0;
            let effective_used_complete = used_complete || existing_used_complete;
            let effective_used_ids = used.map(|reported| {
                if used_complete {
                    reported.to_vec()
                } else {
                    let mut merged = existing_used_ids.clone();
                    let mut seen: HashSet<String> = merged.iter().cloned().collect();
                    merged.extend(
                        reported
                            .iter()
                            .filter(|id| seen.insert((*id).clone()))
                            .cloned(),
                    );
                    merged
                }
            });
            if let Some(used_ids) = effective_used_ids.as_deref() {
                let previously_used: HashSet<String> = existing_used_ids.iter().cloned().collect();
                if used_complete {
                    self.storage.replace_used_trace(
                        trace_id,
                        used_ids,
                        used_strength,
                        used_attribution,
                        source,
                        &now,
                    )?;
                } else if let Some(reported) = used {
                    self.storage.merge_used_trace(
                        trace_id,
                        reported,
                        used_strength,
                        used_attribution,
                        source,
                        &now,
                    )?;
                }
                let affected: HashSet<String> = previously_used
                    .into_iter()
                    .chain(used_ids.iter().cloned())
                    .collect();
                for cid in affected {
                    self.storage.refresh_chunk_last_used(&cid, &now)?;
                }
            }

            // usage_trace: task_ok / task_fail
            if let Some(o) = outcome {
                if matches!(o, "ok" | "fail") {
                    let event = if o == "ok" { "task_ok" } else { "task_fail" };
                    let strength = if event == "task_fail" { 0.15 } else { 1.0 };
                    self.storage.conn_execute(
                        "DELETE FROM usage_trace
                         WHERE trace_id=? AND event IN ('task_ok','task_fail')
                           AND chunk_id IS NULL",
                        rusqlite::params![trace_id],
                    )?;
                    self.storage.insert_usage_trace(
                        trace_id, None, event, strength, None, None, None, None, None, source, &now,
                    )?;
                }
            }

            // Rebuild trace-derived evidence whenever either side of the pair arrives.
            // This makes `outcome → used` and `used → outcome` equivalent and lets a
            // later complete usage declaration replace an earlier one safely.
            let effective_outcome =
                outcome
                    .filter(|value| *value != "unknown")
                    .or(existing_outcome
                        .as_deref()
                        .filter(|value| *value != "unknown"));
            if let Some(o @ ("ok" | "fail")) = effective_outcome {
                if used.is_some()
                    || (outcome.is_some_and(|value| value != "unknown")
                        && existing_outcome.as_deref() != outcome)
                {
                    let fallback_ids: Vec<String>;
                    let effective_used: Option<&[String]> = if effective_used_ids.is_some() {
                        effective_used_ids.as_deref()
                    } else {
                        fallback_ids = log
                            .get("used_ids")
                            .and_then(Value::as_str)
                            .and_then(|s| serde_json::from_str(s).ok())
                            .unwrap_or_default();
                        if fallback_ids.is_empty() {
                            None
                        } else {
                            Some(&fallback_ids)
                        }
                    };
                    let effective_complete = if used.is_some() {
                        effective_used_complete
                    } else {
                        log.get("usage_state").and_then(Value::as_str) != Some("unknown")
                            && log
                                .get("used_complete")
                                .and_then(Value::as_i64)
                                .unwrap_or(1)
                                != 0
                    };
                    self.replace_outcome_evidence(
                        trace_id,
                        o,
                        effective_used,
                        effective_complete,
                        &now,
                    )?;
                }
            } else if used.is_some() && effective_used_complete {
                self.replace_selected_unused_evidence(
                    trace_id,
                    effective_used_ids.as_deref().unwrap_or_default(),
                    &now,
                )?;
            }

            let context_key = log
                .get("context_key")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| query.map(|q| content_hash(&normalize_query(q))));
            let feedback_strength = if feedback_kind == "judge" { 0.6 } else { 1.0 };

            // Persist feedback facts before reducing them into confidence.
            // INSERT OR IGNORE: skip all derived updates for duplicate (trace_id, chunk_id, signal).
            // Track affected chunks so we only rebuild their context_stats (not the full table).
            let mut context_affected: HashSet<String> = HashSet::new();
            if let Some(used_ids) = effective_used_ids.as_deref() {
                for cid in used_ids {
                    context_affected.insert(cid.clone());
                }
            }
            if let Some(ups) = feedback_up {
                for cid in ups {
                    let corrected = self.storage.delete_feedback_event(trace_id, cid, "down")?;
                    self.storage.delete_chunk_trace_confidence_evidence(
                        trace_id,
                        cid,
                        "feedback_down",
                    )?;
                    let inserted = self.storage.insert_feedback_event(
                        &gen_uuid(),
                        trace_id,
                        cid,
                        "up",
                        feedback_strength,
                        source,
                        feedback_actor,
                        feedback_reason,
                        context_key.as_deref(),
                        &now,
                    )?;
                    if inserted > 0 {
                        self.upsert_trace_confidence_evidence(
                            trace_id,
                            cid,
                            "feedback_up",
                            1.0,
                            feedback_strength,
                            if feedback_kind == "judge" {
                                "judge_up"
                            } else {
                                "user_up"
                            },
                            context_key.as_deref(),
                            &now,
                            true,
                        )?;
                        self.storage.update_chunk_last_used(cid, &now)?;
                        self.refresh_governance_evidence(cid, &now)?;
                        context_affected.insert(cid.clone());
                    } else if corrected > 0 {
                        self.recompute_chunk_confidence(cid, &now)?;
                        self.refresh_governance_evidence(cid, &now)?;
                        context_affected.insert(cid.clone());
                    }
                }
            }
            if let Some(downs) = feedback_down {
                for cid in downs {
                    let corrected = self.storage.delete_feedback_event(trace_id, cid, "up")?;
                    self.storage.delete_chunk_trace_confidence_evidence(
                        trace_id,
                        cid,
                        "feedback_up",
                    )?;
                    let inserted = self.storage.insert_feedback_event(
                        &gen_uuid(),
                        trace_id,
                        cid,
                        "down",
                        feedback_strength,
                        source,
                        feedback_actor,
                        feedback_reason,
                        context_key.as_deref(),
                        &now,
                    )?;
                    if inserted > 0 {
                        self.upsert_trace_confidence_evidence(
                            trace_id,
                            cid,
                            "feedback_down",
                            0.0,
                            feedback_strength,
                            if feedback_kind == "judge" {
                                "judge_down"
                            } else {
                                "user_down"
                            },
                            context_key.as_deref(),
                            &now,
                            true,
                        )?;
                        self.refresh_governance_evidence(cid, &now)?;
                        context_affected.insert(cid.clone());
                    } else if corrected > 0 {
                        self.recompute_chunk_confidence(cid, &now)?;
                        self.refresh_governance_evidence(cid, &now)?;
                        context_affected.insert(cid.clone());
                    }
                }
            }
            // Targeted rebuild — only update context_stats for chunks touched in this call.
            self.rebuild_context_stats_for(&context_affected, &now)?;

            // Fill in content fields (補写: output_summary, nomination, output, query) on existing log.
            if !is_fresh_insert {
                self.storage.patch_episodic_log_content(
                    trace_id,
                    query,
                    output,
                    output_summary,
                    nomination,
                    effective_priority,
                )?;
            }

            let lifecycle_state = if effective_outcome.is_some() {
                "completed"
            } else {
                task_state.unwrap_or_else(|| {
                    log.get("task_state")
                        .and_then(Value::as_str)
                        .unwrap_or("running")
                })
            };
            let used_ids_json = effective_used_ids
                .as_deref()
                .map(serde_json::to_string)
                .transpose()?;
            self.storage.update_trace_lifecycle(
                trace_id,
                lifecycle_state,
                (lifecycle_state == "completed").then_some(now.as_str()),
                effective_used_ids
                    .as_deref()
                    .map(|ids| usage_state(Some(ids))),
                used_ids_json.as_deref(),
                used.map(|_| used_attribution),
                used.map(|_| effective_used_complete),
            )?;

            // Update episodic log
            let current_state = log
                .get("distill_state")
                .and_then(Value::as_str)
                .unwrap_or("open");
            let lifecycle_completed = lifecycle_state == "completed";
            let has_material = output_summary.is_some()
                || nomination.is_some()
                || output.is_some()
                || log.get("output_summary").and_then(Value::as_str).is_some()
                || log.get("nomination").and_then(Value::as_str).is_some()
                || log.get("output").and_then(Value::as_str).is_some();
            let retryable_discard = current_state == "discarded"
                && matches!(
                    log.get("distill_note").and_then(Value::as_str),
                    Some("insufficient_material" | "abandoned" | "timed_out")
                );
            let new_state = if current_state == "open"
                && matches!(lifecycle_state, "abandoned" | "timed_out")
            {
                Some("discarded")
            } else if lifecycle_completed && (current_state == "open" || retryable_discard) {
                if has_material {
                    Some("new")
                } else {
                    Some("discarded")
                }
            } else {
                None
            };
            if let Some(state) = new_state {
                let note = if state == "discarded" {
                    Some(if matches!(lifecycle_state, "abandoned" | "timed_out") {
                        lifecycle_state
                    } else {
                        "insufficient_material"
                    })
                } else {
                    None
                };
                let outcome_str = outcome.map(str::to_string);
                self.storage.update_episodic_log_state(
                    trace_id,
                    state,
                    note,
                    outcome_str.as_deref(),
                )?;
                if state == "new" && retryable_discard {
                    self.storage.conn_execute(
                        "UPDATE episodic_log SET distill_note=NULL WHERE trace_id=?",
                        rusqlite::params![trace_id],
                    )?;
                }
            } else if outcome.is_some() {
                let outcome_str = outcome.map(str::to_string);
                self.storage.update_episodic_log_state(
                    trace_id,
                    current_state,
                    None,
                    outcome_str.as_deref(),
                )?;
            }

            self.storage.commit()
        })();
        if result.is_err() {
            let _ = self.storage.rollback();
        }
        result?;
        self.enqueue_evolve_if_needed(&now)?;
        Ok(())
    }
}
