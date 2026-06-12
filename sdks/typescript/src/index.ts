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

export interface EvolveResult {
  distilled: number;
  curate: {
    archived: number;
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
    return this.run<RecallResult>(...args);
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
  }

  async initialize(): Promise<void> {
    await this.call("initialize", {
      protocolVersion: "2024-11-05",
      clientInfo: { name: "innate-ts", version: "0.1.8" },
    });
    this.send({ jsonrpc: "2.0", id: 0, method: "notifications/initialized" });
  }

  private send(msg: McpRequest | object): void {
    this.stdin.write(JSON.stringify(msg) + "\n");
  }

  private call(method: string, params?: unknown): Promise<unknown> {
    return new Promise((resolve, reject) => {
      const id = this.nextId++;
      this.pending.set(id, (resp) => {
        if (resp.error) reject(new Error(resp.error.message));
        else resolve(resp.result);
      });
      this.send({ jsonrpc: "2.0", id, method, params });
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
  } = {}): Promise<RecallResult> {
    return this.toolCall("innate_recall", {
      query,
      budget: options.budget ?? 6000,
      ...(options.top != null ? { top: options.top } : {}),
      source: options.source ?? "sdk",
    }) as Promise<RecallResult>;
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
  } = {}): Promise<void> {
    await this.toolCall("innate_record", { trace_id: traceId, ...options });
  }

  async add(content: string, options: {
    kind?: string; triggerDesc?: string; source?: string;
  } = {}): Promise<string> {
    const r = await this.toolCall("innate_add", {
      content,
      kind: options.kind ?? "note",
      source: options.source ?? "agent",
      ...(options.triggerDesc ? { trigger_desc: options.triggerDesc } : {}),
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

  close(): void {
    this.stdin.end();
    this.proc.kill();
  }
}
