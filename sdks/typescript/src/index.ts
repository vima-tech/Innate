/**
 * Innate TypeScript SDK
 *
 * Two modes:
 *   1. CLI subprocess — call the `innate` binary directly (zero-dependency).
 *   2. MCP client    — connect to `innate mcp` via stdio (for agent integrations).
 *
 * CLI mode is the default for programmatic use.
 */

import { execSync, spawn, SpawnOptions } from "child_process";
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
  params: Record<string, number>;
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

  private run<T>(...args: string[]): T {
    const cmd = [this.binary, ...this.dbArgs, ...args].join(" ");
    try {
      const out = execSync(cmd, { encoding: "utf8" });
      return JSON.parse(out) as T;
    } catch (e: unknown) {
      const err = e as { stderr?: Buffer; message?: string };
      throw new Error(
        `innate error: ${err.stderr?.toString().trim() ?? err.message ?? "unknown"}`
      );
    }
  }

  private runRaw(...args: string[]): string {
    const cmd = [this.binary, ...this.dbArgs, ...args].join(" ");
    return execSync(cmd, { encoding: "utf8" }).trim();
  }

  recall(
    query: string,
    options: {
      budget?: number;
      top?: number;
      includeSparks?: boolean;
      source?: string;
    } = {}
  ): RecallResult {
    const args = [
      "recall",
      JSON.stringify(query),
      "--format",
      "json",
      "--budget",
      String(options.budget ?? 6000),
    ];
    if (options.top != null) args.push("--top", String(options.top));
    if (options.includeSparks) args.push("--include-sparks");
    return this.run<RecallResult>(...args);
  }

  record(
    traceId: string,
    options: {
      outcome?: "ok" | "fail" | "unknown";
      used?: string[];
      outputSummary?: string;
      nomination?: string;
      source?: string;
    } = {}
  ): void {
    const args = ["record", traceId, "--source", options.source ?? "sdk"];
    if (options.outcome) args.push("--outcome", options.outcome);
    if (options.used?.length) args.push("--used", options.used.join(","));
    if (options.outputSummary)
      args.push("--output-summary", JSON.stringify(options.outputSummary));
    if (options.nomination)
      args.push("--nomination", JSON.stringify(options.nomination));
    this.runRaw(...args);
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
      JSON.stringify(content),
      "--kind",
      options.kind ?? "note",
      "--source",
      options.source ?? "agent",
    ];
    if (options.triggerDesc)
      args.push("--trigger", JSON.stringify(options.triggerDesc));
    if (options.antiTriggerDesc)
      args.push("--anti-trigger", JSON.stringify(options.antiTriggerDesc));
    if (options.skillName)
      args.push("--skill-name", JSON.stringify(options.skillName));
    return this.runRaw(...args);
  }

  spark(content: string, options: { triggerDesc?: string } = {}): string {
    const args = ["spark", JSON.stringify(content)];
    if (options.triggerDesc)
      args.push("--trigger", JSON.stringify(options.triggerDesc));
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
      clientInfo: { name: "innate-ts", version: "0.2.0" },
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
    outcome?: string; used?: string[];
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
