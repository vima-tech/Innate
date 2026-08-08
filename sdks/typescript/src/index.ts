/**
 * Innate TypeScript SDK
 *
 * Two modes:
 *   1. CLI subprocess — call the `innate` binary directly (zero-dependency).
 *   2. MCP client    — connect to `innate mcp` via stdio (for agent integrations).
 *
 * CLI mode is the default for programmatic use.
 */

import { execFileSync, spawn, SpawnOptions } from "child_process";
import { Readable, Writable } from "stream";

export interface Chunk {
  id: string;
  content: string;
  trigger_desc?: string;
  anti_trigger_desc?: string;
  confidence: number;
  origin: string;
  state: string;
  maturity?: string;
  token_count?: number;
  [key: string]: unknown;
}

export interface RecallResult {
  trace_id: string;
  knowledge: Chunk[];
  sparks: Chunk[];
  empty: boolean;
}

export interface InspectResult {
  schema_version: string;
  lib_id: string;
  last_agg_ts: string;
  chunks: { total: number; active: number; pending: number; archived: number };
  sparks: number;
  episodic_log: { open: number; new: number };
  embed_rebuild_queue: number;
  feedback_loop: {
    trace_completion_rate: number;
    usage_annotation_rate: number;
    trace_use_rate: number;
    selected_to_used_rate: number;
    task_success_rate: number;
    feedback_coverage: number;
    feedback_events: number;
    timed_out_traces: number;
    pending_evolve_requests: number;
    failed_evolve_requests_30d: number;
    failed_distill_logs_30d: number;
    pending_governance_proposals: number;
    window_days: number;
    confidence_distribution: { low: number; medium: number; high: number };
  };
  params: Record<string, number>;
  /** Observability blocks (schema 4.19–4.21). Optional: a sub-block is omitted until
   *  its data exists (no operation_runs yet → operational.ops absent; no snapshot →
   *  trends absent), so consumers must treat each as possibly-undefined. */
  observability?: ObservabilityBlock;
  operational?: OperationalBlock;
  trends?: TrendsBlock;
}

/** Per-window / per-dimension derived metrics + recall-pack + lifecycle (design §5.1). */
export interface ObservabilityBlock {
  windows: Record<string, RecallRates>;
  by_dimension: {
    event_source: { agent_coverage: Record<string, unknown>; rates: Record<string, RecallRates> };
    agent: Record<string, RecallRates>;
    context_key: Record<string, RecallRates>;
  };
  recall_pack: {
    zombie_chunks: number;
    avg_retrieved: number;
    avg_selected: number;
    selected_unused_rate: number;
    selected_unused_top: Array<{ id: string; selected_count: number }>;
    used_rank_mrr: number;
    hook_silence_rate: number;
    selected_rank_distribution: Record<"1" | "2-3" | "4-10" | "11+", number>;
    high_rank_unused: Array<{ id: string; best_rank: number }>;
    low_rank_used: Array<{ id: string; best_rank: number }>;
  };
  lifecycle: {
    pending_oldest_ts: string | null;
    governance_backlog_oldest_ts: string | null;
    state_transition_approx: { promotions_7d: number; evictions_7d: number; note: string };
  };
}

export interface RecallRates {
  recalls: number;
  empty_recall_rate: number;
  completed_rate: number;
  task_success_rate: number;
  usage_annotation_rate?: number;
  selected_to_used_rate?: number;
  feedback_coverage?: number;
  timeout_rate?: number;
}

/** Daemon health (independent read-only connection) + operation_runs aggregation. */
export interface OperationalBlock {
  daemon: {
    state: string;
    running?: boolean;
    pid?: number | null;
    errors_24h?: number;
    errors_7d?: number;
    errors_by_operation?: Record<string, number>;
    last_error?: unknown;
    error?: string;
  };
  /** Present only once operation_runs has rows. */
  ops?: {
    by_op: Record<string, OpPerf>;
    /** Latency/success broken down by source / agent / context (context is
     *  trace-derived, so only covers ops carrying a trace_id). */
    by_source: Record<string, OpPerf>;
    by_agent: Record<string, OpPerf>;
    by_context: Record<string, OpPerf>;
    error_kind_top: Array<{ error_kind: string; count: number }>;
  };
}

export interface OpPerf {
  count: number;
  ok: number;
  error: number;
  timeout: number;
  success_rate: number;
  p50_ms: number;
  p95_ms: number;
}

/** Week-over-week KPI deltas from metric_snapshots. */
export interface TrendsBlock {
  current_ts: string;
  current: Record<string, number>;
  baseline_ts?: string;
  delta_vs_7d?: Record<string, number>;
}

export interface FlaggedPoint {
  chunk_id: string;
  summary: string;
  resonance: number;
  calibration: number;
  strength: number;
}

export interface Contributor {
  chunk_id: string;
  valence: "affirm" | "caution" | "mixed" | "neutral";
  strength: number;
}

/**
 * Critic judgement from `appraise`. Carries no answer text — only footing
 * (`strength`/`tier`), polarity (`valence`), and things to be careful about
 * (`flagged_points`).
 */
export type AbstainReason =
  | "weak_resonance"
  | "false_resonance"
  | "sparse_evidence"
  | "conflicted";

export interface Verdict {
  /** Fixed declaration returned with every verdict: intuition is a reference
   *  signal only, not a precise answer. Weigh it; never let it override analysis. */
  advisory: string;
  valence: "affirm" | "caution" | "mixed" | "neutral";
  strength: number;
  tier: "weak" | "medium" | "strong";
  /** Scheme E/G: calibrated, dispersion-shaped confidence ∈ [0,1]. */
  confidence: number;
  /** Scheme G: top-k neighbour fused dispersion (max−min). */
  dispersion: number;
  /** Scheme A: abstaining is a first-class, correct output — not a failure. */
  abstained: boolean;
  abstain_reason?: AbstainReason | null;
  flagged_points: FlaggedPoint[];
  contributors: Contributor[];
  trace_id: string;
}

export interface Situation {
  query?: string;
  lastError?: string;
  recentActions?: string[];
  stage?: string;
  fileContext?: string;
}

export interface TracedResult<T> {
  result: T;
  outcome?: "ok" | "fail" | "unknown";
  used?: string[];
  usedAttribution?: "explicit" | "cited" | "inferred";
  usedComplete?: boolean;
  outputSummary?: string;
  nomination?: string;
}

export interface BackupOptions {
  /** run=backup now, status=show last backup state, list=list R2 backups, prune=delete old backups. Defaults to "run". */
  action?: "run" | "status" | "list" | "prune";
  /** For action=run: skip the interval check and backup immediately (default false). */
  force?: boolean;
}

export interface EvolveResult {
  distilled: number;
  curate: {
    archived: number;
    promoted: number;
    deduped: number;
    decayed: number;
    recovered: number;
    orphans: number;
    warnings: string[];
  } | null;
}

// ---------------------------------------------------------------------------
// CLI subprocess client (default)
// ---------------------------------------------------------------------------

export interface KnowledgeBaseOptions {
  dbPath?: string;
  binary?: string;
}

export class KnowledgeBase {
  private readonly binary: string;
  private readonly dbArgs: string[];

  constructor(options: KnowledgeBaseOptions = {}) {
    this.binary = options.binary ?? process.env.INNATE_BIN ?? "innate";
    const db = options.dbPath ?? process.env.INNATE_DB;
    this.dbArgs = db ? ["--db", db] : [];
  }

  // Use execFileSync (no shell) to avoid injection and quoting issues.
  private run<T>(...args: string[]): T {
    try {
      const out = execFileSync(this.binary, [...this.dbArgs, ...args], { encoding: "utf8" });
      return JSON.parse(out) as T;
    } catch (e: unknown) {
      const err = e as { stderr?: Buffer; message?: string };
      throw new Error(
        `innate error: ${err.stderr?.toString().trim() ?? err.message ?? "unknown"}`
      );
    }
  }

  private runRaw(...args: string[]): string {
    return execFileSync(this.binary, [...this.dbArgs, ...args], { encoding: "utf8" }).trim();
  }

  recall(
    query: string,
    options: {
      budget?: number;
      top?: number;
      includeSparks?: boolean;
      source?: string;
      expandDeps?: string;
      allowTrim?: boolean;
      refineMode?: string;
      rerank?: boolean;
    } = {}
  ): RecallResult {
    const args = [
      "recall",
      query,
      "--format",
      "json",
      "--budget",
      String(options.budget ?? 6000),
      "--source",
      options.source ?? "sdk",
      "--expand-deps",
      options.expandDeps ?? "false",
      "--refine-mode",
      options.refineMode ?? "off",
    ];
    if (options.top != null) args.push("--top", String(options.top));
    if (options.includeSparks) args.push("--include-sparks");
    if (options.allowTrim) args.push("--allow-trim");
    if (options.rerank) args.push("--rerank");
    return this.run<RecallResult>(...args);
  }

  /**
   * Critic check: judge how much footing exists for a candidate in a situation.
   * Returns a {@link Verdict} — never an answer. Pair with `record({ feedback: "down" })`
   * on `verdict.trace_id` to flow an override back.
   */
  appraise(
    situation: Situation = {},
    options: {
      candidate?: string;
      top?: number;
      minStrength?: number;
      source?: string;
    } = {}
  ): Verdict {
    const args = ["appraise", "--format", "json", "--source", options.source ?? "sdk"];
    if (situation.query) args.push("--query", situation.query);
    if (situation.lastError) args.push("--last-error", situation.lastError);
    if (situation.recentActions?.length)
      args.push("--recent-actions", situation.recentActions.join(","));
    if (situation.stage) args.push("--stage", situation.stage);
    if (situation.fileContext) args.push("--file-context", situation.fileContext);
    if (options.candidate) args.push("--candidate", options.candidate);
    if (options.top != null) args.push("--top", String(options.top));
    if (options.minStrength != null) args.push("--min-strength", String(options.minStrength));
    return this.run<Verdict>(...args);
  }

  record(
    traceId: string,
    options: {
      query?: string;
      outcome?: "ok" | "fail" | "unknown";
      used?: string[];
      usedAttribution?: "explicit" | "cited" | "inferred";
      usedComplete?: boolean;
      output?: string;
      outputSummary?: string;
      nomination?: string;
      feedback?: "up" | "down";
      feedbackKind?: "user" | "judge";
      feedbackActor?: string;
      feedbackReason?: string;
      taskState?: "recalled" | "running" | "completed" | "abandoned" | "timed_out";
      verdictHeeded?: boolean;
      source?: string;
    } = {}
  ): void {
    const args = ["record", traceId, "--source", options.source ?? "sdk"];
    if (options.query) args.push("--query", options.query);
    if (options.outcome) args.push("--outcome", options.outcome);
    if (options.used !== undefined) {
      args.push(
        "--used",
        options.used.join(","),
        "--used-attribution",
        options.usedAttribution ?? "explicit"
      );
      if (options.usedComplete === false) args.push("--used-partial");
    }
    if (options.output) args.push("--output", options.output);
    if (options.outputSummary) args.push("--output-summary", options.outputSummary);
    if (options.nomination) args.push("--nomination", options.nomination);
    if (options.feedback) {
      args.push("--feedback", options.feedback, "--feedback-kind", options.feedbackKind ?? "user");
    }
    if (options.feedbackActor) args.push("--feedback-actor", options.feedbackActor);
    if (options.feedbackReason) args.push("--feedback-reason", options.feedbackReason);
    if (options.taskState) args.push("--task-state", options.taskState);
    if (options.verdictHeeded) args.push("--verdict-heeded");
    this.runRaw(...args);
  }

  withTrace<T>(
    query: string,
    fn: (context: RecallResult) => T | TracedResult<T>,
    options: Parameters<KnowledgeBase["recall"]>[1] = {}
  ): T {
    const source = options.source ?? "augmented";
    const context = this.recall(query, { ...options, source });
    this.record(context.trace_id, {
      taskState: "running",
      source,
    });
    try {
      const value = fn(context);
      if (typeof value === "object" && value !== null && "result" in value) {
        const traced = value as TracedResult<T>;
        this.record(context.trace_id, {
          outcome: traced.outcome ?? "ok",
          used: traced.used,
          usedAttribution: traced.usedAttribution,
          usedComplete: traced.usedComplete,
          outputSummary: traced.outputSummary,
          nomination: traced.nomination,
          source,
        });
        return traced.result;
      }
      this.record(context.trace_id, {
        outcome: "ok",
        source,
      });
      return value as T;
    } catch (error) {
      this.record(context.trace_id, {
        outcome: "fail",
        outputSummary: error instanceof Error ? `${error.name}: ${error.message}` : String(error),
        source,
      });
      throw error;
    }
  }

  add(
    content: string,
    options: {
      kind?: "note" | "skill";
      triggerDesc?: string;
      antiTriggerDesc?: string;
      source?: "chat" | "manual" | "doc" | "agent";
      skillName?: string;
      dependsOn?: string[];
      depKind?: "hard" | "soft";
    } = {}
  ): string {
    const args = [
      "add",
      content,
      "--kind",
      options.kind ?? "note",
      "--source",
      options.source ?? "agent",
    ];
    if (options.triggerDesc) args.push("--trigger", options.triggerDesc);
    if (options.antiTriggerDesc) args.push("--anti-trigger", options.antiTriggerDesc);
    if (options.skillName) args.push("--skill-name", options.skillName);
    // Dependencies: matches the MCP `add` and the CLI's `--depends-on` (repeatable)
    // + `--dep-kind`, so the subprocess SDK is no longer missing edge support.
    if (options.depKind) args.push("--dep-kind", options.depKind);
    for (const dst of options.dependsOn ?? []) args.push("--depends-on", dst);
    return this.runRaw(...args);
  }

  spark(content: string, options: { triggerDesc?: string } = {}): string {
    const args = ["spark", content];
    if (options.triggerDesc) args.push("--trigger", options.triggerDesc);
    return this.runRaw(...args);
  }

  evolve(trigger: "manual" | "scheduled" | "threshold" = "manual"): EvolveResult {
    return this.run<EvolveResult>("evolve", "--trigger", trigger);
  }

  inspect(): InspectResult {
    return this.run<InspectResult>("inspect");
  }

  approve(chunkId: string): void {
    this.runRaw("approve", chunkId);
  }

  archive(chunkId: string, reason = "stale"): void {
    this.runRaw("archive", chunkId, "--reason", reason);
  }

  invalidate(chunkId: string, reason = ""): void {
    const args = ["invalidate", chunkId];
    if (reason) args.push("--reason", reason);
    this.runRaw(...args);
  }

  restore(chunkId: string): void {
    this.runRaw("restore", chunkId);
  }

  matureSpark(sparkId: string, to: "sprouting" | "incubating"): void {
    this.runRaw("mature-spark", sparkId, to);
  }

  promoteSpark(sparkId: string, to: "note" | "skill" = "note"): string {
    return this.runRaw("promote-spark", sparkId, "--to", to);
  }

  dropSpark(sparkId: string, reason = ""): void {
    const args = ["drop-spark", sparkId];
    if (reason) args.push("--reason", reason);
    this.runRaw(...args);
  }
}

// ---------------------------------------------------------------------------
// MCP stdio client (for agent/host integrations)
// ---------------------------------------------------------------------------

interface McpRequest {
  jsonrpc: "2.0";
  id: number;
  method: string;
  params?: unknown;
}

interface McpResponse {
  jsonrpc: "2.0";
  id: number;
  result?: unknown;
  error?: { code: number; message: string };
}

export class McpClient {
  private proc: ReturnType<typeof spawn>;
  private stdin: Writable;
  private pending = new Map<number, (r: McpResponse) => void>();
  private nextId = 1;
  private buf = "";

  constructor(options: KnowledgeBaseOptions = {}) {
    const binary = options.binary ?? process.env.INNATE_BIN ?? "innate";
    const args = ["mcp"];
    if (options.dbPath) args.push("--db", options.dbPath);
    else if (process.env.INNATE_DB) args.push("--db", process.env.INNATE_DB);

    this.proc = spawn(binary, args, {
      stdio: ["pipe", "pipe", "inherit"],
    } as SpawnOptions);
    this.stdin = this.proc.stdin!;
    const stdout = this.proc.stdout as Readable;

    stdout.on("data", (chunk: Buffer) => {
      this.buf += chunk.toString();
      const lines = this.buf.split("\n");
      this.buf = lines.pop() ?? "";
      for (const line of lines) {
        if (!line.trim()) continue;
        try {
          const resp = JSON.parse(line) as McpResponse;
          const cb = this.pending.get(resp.id);
          if (cb) { this.pending.delete(resp.id); cb(resp); }
        } catch { /* ignore malformed */ }
      }
    });

    // Fail every in-flight request if the subprocess dies or errors, so callers
    // never hang on a Promise that can no longer be answered.
    this.proc.on("exit", (code) =>
      this.failAll(new Error(`innate mcp process exited (code ${code ?? "unknown"})`)),
    );
    this.proc.on("error", (err) =>
      this.failAll(new Error(`innate mcp process error: ${err.message}`)),
    );
    // A broken stdin pipe (e.g. the child exited) surfaces as an async 'error'
    // on the stream. Without a listener Node would throw it as uncaught and
    // crash the host process; instead fail every pending request.
    this.stdin.on("error", (err) =>
      this.failAll(new Error(`innate mcp stdin error: ${err.message}`)),
    );
  }

  private failAll(err: Error): void {
    for (const cb of this.pending.values()) {
      cb({ jsonrpc: "2.0", id: -1, error: { code: -1, message: err.message } });
    }
    this.pending.clear();
  }

  async initialize(): Promise<void> {
    await this.call("initialize", {
      protocolVersion: "2024-11-05",
      clientInfo: { name: "innate-ts", version: "0.1.10" },
    });
    this.send({ jsonrpc: "2.0", id: 0, method: "notifications/initialized" });
  }

  private send(msg: McpRequest | object): void {
    // Surface async write failures (back-pressure / EPIPE) instead of dropping
    // them: a failed write means the request will never be answered, so fail
    // all in-flight calls rather than letting them hang until timeout.
    this.stdin.write(JSON.stringify(msg) + "\n", (err) => {
      if (err) this.failAll(new Error(`innate mcp stdin write failed: ${err.message}`));
    });
  }

  private call(method: string, params?: unknown, timeoutMs = 30000): Promise<unknown> {
    return new Promise((resolve, reject) => {
      const id = this.nextId++;
      const timer = setTimeout(() => {
        if (this.pending.delete(id)) {
          reject(new Error(`innate mcp request timed out after ${timeoutMs}ms: ${method}`));
        }
      }, timeoutMs);
      this.pending.set(id, (resp) => {
        clearTimeout(timer);
        if (resp.error) reject(new Error(resp.error.message));
        else resolve(resp.result);
      });
      try {
        this.send({ jsonrpc: "2.0", id, method, params });
      } catch (e) {
        clearTimeout(timer);
        this.pending.delete(id);
        reject(e instanceof Error ? e : new Error(String(e)));
      }
    });
  }

  async toolCall(name: string, args: Record<string, unknown>): Promise<unknown> {
    const result = await this.call("tools/call", { name, arguments: args });
    const r = result as { content?: Array<{ text?: string }>; isError?: boolean };
    const text = r.content?.[0]?.text ?? "";
    if (r.isError) throw new Error(text);
    try { return JSON.parse(text); } catch { return text; }
  }

  async recall(query: string, options: {
    budget?: number; top?: number; source?: string;
    includeSparks?: boolean;
    expandDeps?: "false" | "direct" | "closure";
    allowTrim?: boolean;
    refineMode?: "off" | "trim" | "adapt";
  } = {}): Promise<RecallResult> {
    return this.toolCall("innate_recall", {
      query,
      budget: options.budget ?? 6000,
      ...(options.top != null ? { top: options.top } : {}),
      source: options.source ?? "sdk",
      ...(options.includeSparks != null ? { include_sparks: options.includeSparks } : {}),
      ...(options.expandDeps != null ? { expand_deps: options.expandDeps } : {}),
      ...(options.allowTrim != null ? { allow_trim: options.allowTrim } : {}),
      ...(options.refineMode != null ? { refine_mode: options.refineMode } : {}),
    }) as Promise<RecallResult>;
  }

  async appraise(situation: Situation = {}, options: {
    candidate?: string;
    top?: number;
    minStrength?: number;
    source?: string;
  } = {}): Promise<Verdict> {
    return this.toolCall("innate_appraise", {
      ...(situation.query ? { query: situation.query } : {}),
      ...(situation.lastError ? { last_error: situation.lastError } : {}),
      ...(situation.recentActions?.length ? { recent_actions: situation.recentActions } : {}),
      ...(situation.stage ? { stage: situation.stage } : {}),
      ...(situation.fileContext ? { file_context: situation.fileContext } : {}),
      ...(options.candidate ? { candidate: options.candidate } : {}),
      ...(options.top != null ? { top: options.top } : {}),
      ...(options.minStrength != null ? { min_strength: options.minStrength } : {}),
      source: options.source ?? "sdk",
    }) as Promise<Verdict>;
  }

  async record(traceId: string, options: {
    outcome?: string;
    used?: string[];
    used_attribution?: "explicit" | "cited" | "inferred";
    used_complete?: boolean;
    feedback_up?: string[];
    feedback_down?: string[];
    feedback_kind?: "user" | "judge";
    feedback_actor?: string;
    feedback_reason?: string;
    task_state?: string;
    verdict_heeded?: boolean;
  } = {}): Promise<void> {
    await this.toolCall("innate_record", { trace_id: traceId, ...options });
  }

  async add(content: string, options: {
    kind?: string; triggerDesc?: string; source?: string;
    dependsOn?: string[]; depKind?: "hard" | "soft";
  } = {}): Promise<string> {
    const r = await this.toolCall("innate_add", {
      content,
      kind: options.kind ?? "note",
      source: options.source ?? "agent",
      ...(options.triggerDesc ? { trigger_desc: options.triggerDesc } : {}),
      ...(options.dependsOn?.length ? { depends_on: options.dependsOn } : {}),
      ...(options.depKind ? { dep_kind: options.depKind } : {}),
    }) as { chunk_id: string };
    return r.chunk_id;
  }

  async spark(content: string): Promise<string> {
    const r = await this.toolCall("innate_spark", { content }) as { chunk_id: string };
    return r.chunk_id;
  }

  async inspect(): Promise<InspectResult> {
    return this.toolCall("innate_inspect", {}) as Promise<InspectResult>;
  }

  // ── Governance & spark lifecycle (typed wrappers over the MCP tools) ────────
  // Previously only reachable via the generic `toolCall` escape hatch; these
  // typed methods mirror the CLI `KnowledgeBase` surface for parity.

  async evolve(options: { trigger?: string; rebuildEmbeddings?: boolean } = {}): Promise<EvolveResult> {
    return this.toolCall("innate_evolve", {
      trigger: options.trigger ?? "manual",
      ...(options.rebuildEmbeddings != null ? { rebuild_embeddings: options.rebuildEmbeddings } : {}),
    }) as Promise<EvolveResult>;
  }

  async approve(chunkId: string): Promise<void> {
    await this.toolCall("innate_approve", { chunk_id: chunkId });
  }

  async archive(chunkId: string, reason = "stale"): Promise<void> {
    await this.toolCall("innate_archive", { chunk_id: chunkId, reason });
  }

  async invalidate(chunkId: string, reason = ""): Promise<void> {
    await this.toolCall("innate_invalidate", { chunk_id: chunkId, reason });
  }

  async restore(chunkId: string): Promise<void> {
    await this.toolCall("innate_restore", { chunk_id: chunkId });
  }

  async matureSpark(sparkId: string, to: string): Promise<void> {
    await this.toolCall("innate_mature_spark", { spark_id: sparkId, to });
  }

  async promoteSpark(sparkId: string, to = "note"): Promise<string> {
    const r = await this.toolCall("innate_promote_spark", { spark_id: sparkId, to }) as { chunk_id: string };
    return r.chunk_id;
  }

  async dropSpark(sparkId: string, reason = ""): Promise<void> {
    await this.toolCall("innate_drop_spark", { spark_id: sparkId, reason });
  }

  /**
   * Backup or inspect Cloudflare R2 backups via the `innate_backup` MCP tool.
   * Mirrors the CLI `innate backup <action>` surface. The shape of the resolved
   * value depends on `action` (run/status/list/prune), so it is returned as
   * `unknown` — narrow it at the call site.
   */
  async backup(options: BackupOptions = {}): Promise<unknown> {
    const action = options.action ?? "run";
    return this.toolCall("innate_backup", { action, force: options.force ?? false });
  }

  close(): void {
    this.stdin.end();
    this.proc.kill();
  }
}
