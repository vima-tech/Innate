"use strict";

// ════════════════════════════════════════════════════════════════════════
// Innate Console — localhost governance console for the procedural KB.
// Vanilla JS, no framework. Strict-CSP safe: no inline scripts/styles/handlers;
// dynamic geometry (meter widths, bar colors) is set via the CSSOM (element
// .style), which CSP style-src does not block.
// ════════════════════════════════════════════════════════════════════════

// Token arrives in the URL *fragment* (#token=…), not the query string: the
// fragment is never sent to the server, never written to access logs, and never
// leaks via the Referer header. We read it once, keep it only in memory, and
// immediately strip it from the address bar so a casual copy of the URL (or a
// later XSS reading location) cannot recover it.
function takeToken() {
  const h = new URLSearchParams((location.hash || "").replace(/^#/, ""));
  const t = h.get("token") || "";
  if (t) history.replaceState(null, "", location.pathname + location.search);
  return t;
}
const TOKEN = takeToken();

const $ = (id) => document.getElementById(id);

// ── App state ───────────────────────────────────────────────────────────────
const LIMIT = 50;
let offset = 0;
let lastCount = 0;          // rows in the most recent chunk page (drives the pager)
let selectedId = null;      // selected chunk id
let selChunkState = null;   // state of the currently-displayed chunk (for the `A` shortcut)
let lastDetail = null;      // last chunk detail payload, so the raw-JSON toggle can re-render
let showRaw = false;        // chunk detail: raw-JSON section expanded?
let govMode = false;        // review-queue mode (replaces the chunk list)
let overviewOpen = false;   // expandable health dashboard open?
let lastInspect = null;     // last /api/inspect payload (feeds health strip + overview)
let lastGovPending = 0;     // review-queue backlog count
let dialog = null;          // { id, action } when the archive/invalidate sheet is open
let currentChunkIds = [];   // ids in current list order (arrow-key navigation)
let currentTraces = [];     // traces in current list order
let selectedTraceIdx = -1;  // selected trace index

// ── i18n ──────────────────────────────────────────────────────────────────
// String values are plain text; function values take args (counts, names). The
// `data-i18n` / `data-i18n-title` DOM attributes cover static chrome; t() covers
// everything rendered from JS. Technical enum values (pending, http_4xx, …) stay
// literal in filters — they mirror API data, not prose.
const I18N = {
  en: {
    subtitle: "knowledge base",
    tab_knowledge: "Knowledge", tab_traces: "LLM Traces",
    tab_search: "Search", tab_play: "Playground", tab_sessions: "Sessions", tab_daemon: "Daemon",
    search_hint: "Type a query and press Enter.", search_empty: "No matches.",
    play_hint: "Run a recall to inspect the fused ranking.", play_empty: "No knowledge recalled.",
    f_budget: "budget", f_sparks: "sparks", k_fused: "fused score",
    sessions_title: "Recent trace timeline", sessions_hint: "Select a trace to view its detail.",
    sessions_empty: "No traces yet.", daemon_title: "Daemon status",
    search_failed: "search failed", play_failed: "recall failed",
    sessions_failed: "sessions failed", daemon_failed: "daemon status failed",
    health_chunks: "chunks", health_debt: "debt", health_pending: "pending",
    health_oldest: (d) => `oldest ${d}d`, health_review: "review queue",
    review_title: "Review queue", exit: "Exit",
    review_banner: "Chunks flagged by repeated negative feedback. Filters are disabled — click an item to adjudicate.",
    f_state: "state", f_origin: "origin", f_kind: "kind", f_status: "status", opt_all: "all",
    prev: "‹ Prev", next: "Next ›", reload: "Reload",
    detail_placeholder: "Select an item to view its detail.",
    trace_placeholder: "Select a call to view its request & response.",
    empty_title: "No item selected",
    theme_toggle_title: "Toggle light / dark theme",
    inspect_failed: "inspect failed",
    obs_title: "Observability",
    obs_errors_24h: "errors 24h",
    obs_embed_p95: (x) => "embed p95 " + x,
    obs_empty_recall: "empty recall 7d",
    obs_task_success: "task success",
    obs_mrr: "used-rank MRR",
    obs_zombie: "zombie",
    obs_no_anomalies: "no anomalies — healthy",
    anom_zombie: (n) => n + " zombie chunk(s): selected but never used",
    anom_empty: (p) => "empty recall rate " + p + " (7d)",
    anom_daemon: (n) => n + " daemon error(s) in 24h",
    anom_slow: (name, x) => "slow op " + name + " p95 " + x + "ms",
    gov_empty: "No chunks flagged for review.",
    flagged: (n) => `${n} flagged`, score: "score", actors: (n) => `${n} actors`,
    chunk_missing: "(chunk missing)",
    no_results: "no results", no_more: "no more results",
    governance: "governance",
    act_approve: "Approve", act_restore: "Restore", act_archive: "Archive…", act_invalidate: "Invalidate…",
    k_id: "id", k_origin: "origin", k_confidence: "confidence", k_created: "created",
    k_last_used: "last used", k_used_selected: "used / selected", hit_rate: "hit rate",
    h_content: "content", show_raw: "Show raw JSON", hide_raw: "Hide raw JSON",
    reason_label: "Reason", cancel: "Cancel",
    confirm_archive: "Archive chunk", confirm_invalidate: "Invalidate chunk",
    archive_desc: "Move this chunk out of active retrieval. It stays restorable. A reason is required for the audit log.",
    invalidate_desc: "Mark this chunk as wrong and exclude it permanently. It stays restorable. A reason is required.",
    reason_placeholder: "e.g. superseded by distill #2; low retrieval quality",
    action_ok: (a) => `${a} ok`, action_failed: (a, e) => `${a} failed: ${e}`,
    load_failed: "load failed", traces_failed: "traces failed", queue_failed: "review queue failed",
    detail_failed: "detail failed",
    trace_empty: "No LLM calls traced yet. Trigger a recall / evolve, then reload.",
    calls: (n) => `${n} calls`, tries: (n) => `${n} try`, tok: (n) => `${n} tok`,
    k_time: "time", k_model: "model", k_host: "host", k_latency: "latency",
    k_attempts: "attempts", k_tokens: "tokens",
    h_error: "error", h_request: "request", h_response: "response", none: "(none)",
    ov_composition: "composition", ov_debt: "knowledge debt", ov_debt_sub: "(pending+archived)/active",
    ov_rebuild: "rebuild queue", ov_rebuild_sub: "re-embed jobs",
    ov_distill: "distill cost (est.)", ov_distill_sub: (n) => `next batch · ${n} entries`,
    st_active: "active", st_pending: "pending", st_archived: "archived", st_invalidated: "invalidated",
  },
  zh: {
    subtitle: "知识库",
    tab_knowledge: "知识", tab_traces: "LLM 调用",
    tab_search: "搜索", tab_play: "调试台", tab_sessions: "会话", tab_daemon: "守护进程",
    search_hint: "输入查询并回车。", search_empty: "无匹配。",
    play_hint: "运行召回以检查融合排序。", play_empty: "未召回任何知识。",
    f_budget: "预算", f_sparks: "火花", k_fused: "融合分",
    sessions_title: "最近轨迹时间线", sessions_hint: "选择一条轨迹查看详情。",
    sessions_empty: "暂无轨迹。", daemon_title: "守护进程状态",
    search_failed: "搜索失败", play_failed: "召回失败",
    sessions_failed: "会话加载失败", daemon_failed: "守护进程状态失败",
    health_oldest: (d) => `最久 ${d} 天`, health_review: "复审队列",
    review_title: "复审队列", exit: "退出",
    review_banner: "被反复负反馈标记的知识块。筛选已禁用 —— 点击条目进行裁决。",
    f_state: "状态", f_origin: "来源", f_kind: "类型", f_status: "状态", opt_all: "全部",
    prev: "‹ 上一页", next: "下一页 ›", reload: "刷新",
    detail_placeholder: "选择一个条目查看详情。",
    trace_placeholder: "选择一次调用查看其请求与响应。",
    empty_title: "未选择条目",
    theme_toggle_title: "切换 明亮 / 暗黑 主题",
    inspect_failed: "巡检失败",
    obs_title: "可观测性",
    obs_errors_24h: "24h 错误",
    obs_embed_p95: (x) => "嵌入 p95 " + x,
    obs_empty_recall: "空召回 7d",
    obs_task_success: "任务成功率",
    obs_mrr: "命中排名 MRR",
    obs_zombie: "僵尸",
    obs_no_anomalies: "无异常 — 健康",
    anom_zombie: (n) => n + " 个僵尸 chunk:被召回却从不采用",
    anom_empty: (p) => "空召回率 " + p + "(7d)",
    anom_daemon: (n) => n + " 个 daemon 错误(24h)",
    anom_slow: (name, x) => "慢操作 " + name + " p95 " + x + "ms",
    gov_empty: "没有待复审的知识块。",
    flagged: (n) => `${n} 项待复审`, score: "评分", actors: (n) => `${n} 个来源`,
    chunk_missing: "(知识块缺失)",
    no_results: "无结果", no_more: "没有更多了",
    governance: "治理",
    act_approve: "批准", act_restore: "恢复", act_archive: "归档…", act_invalidate: "作废…",
    k_id: "ID", k_origin: "来源", k_confidence: "置信度", k_created: "创建于",
    k_last_used: "最近使用", k_used_selected: "使用 / 选中", hit_rate: "命中率",
    h_content: "内容", show_raw: "显示原始 JSON", hide_raw: "隐藏原始 JSON",
    reason_label: "原因", cancel: "取消",
    confirm_archive: "归档知识块", confirm_invalidate: "作废知识块",
    archive_desc: "将此知识块移出活跃检索，可随时恢复。审计日志要求填写原因。",
    invalidate_desc: "将此知识块标记为错误并永久排除，可随时恢复。必须填写原因。",
    reason_placeholder: "例如：已被 distill #2 取代；检索质量低",
    action_ok: (a) => `${a} 成功`, action_failed: (a, e) => `${a} 失败:${e}`,
    load_failed: "加载失败", traces_failed: "调用记录加载失败", queue_failed: "复审队列加载失败",
    detail_failed: "详情加载失败",
    trace_empty: "暂无 LLM 调用记录。触发一次 recall / evolve 后再刷新。",
    calls: (n) => `${n} 次调用`, tries: (n) => `${n} 次尝试`, tok: (n) => `${n} tok`,
    k_time: "时间", k_model: "模型", k_host: "主机", k_latency: "延迟",
    k_attempts: "尝试次数", k_tokens: "token",
    h_error: "错误", h_request: "请求", h_response: "响应", none: "(无)",
    ov_composition: "构成", ov_debt: "知识债务", ov_debt_sub: "(待审+归档)/活跃",
    ov_rebuild: "重建队列", ov_rebuild_sub: "重嵌入任务",
    ov_distill: "蒸馏成本（估）", ov_distill_sub: (n) => `下一批 · ${n} 条`,
    st_active: "活跃", st_pending: "待审核", st_archived: "已归档", st_invalidated: "已失效",
  },
};

let LANG = localStorage.getItem("innate-lang");
if (LANG !== "zh" && LANG !== "en") {
  LANG = (navigator.language || "").toLowerCase().startsWith("zh") ? "zh" : "en";
}

function t(key, ...args) {
  const dict = I18N[LANG] || I18N.en;
  let v = dict[key];
  if (v == null) v = I18N.en[key];
  if (v == null) return key;
  return typeof v === "function" ? v(...args) : v;
}

// Translate all static markup carrying data-i18n / data-i18n-title attributes.
function applyStatic() {
  document.documentElement.lang = LANG === "zh" ? "zh-CN" : "en";
  document.querySelectorAll("[data-i18n]").forEach((el) => { el.textContent = t(el.dataset.i18n); });
  document.querySelectorAll("[data-i18n-title]").forEach((el) => { el.title = t(el.dataset.i18nTitle); });
  updateToggles();
}

// Single-button toggles: each shows the state it switches *to*.
function updateToggles() {
  const lt = $("lang-toggle");
  lt.textContent = LANG === "en" ? "中" : "EN";
  lt.title = LANG === "en" ? "切换到中文" : "Switch to English";
  const tt = $("theme-toggle");
  tt.textContent = THEME === "dark" ? "☀" : "☾";
  tt.title = t("theme_toggle_title");
}

function setLang(lang) {
  if (lang === LANG) return;
  LANG = lang;
  localStorage.setItem("innate-lang", lang);
  applyStatic();
  loadHealth();
  if (onTraces()) {
    loadTraces();
    if (selectedTraceIdx >= 0 && currentTraces[selectedTraceIdx]) renderTraceDetail(currentTraces[selectedTraceIdx]);
  } else if (govMode) {
    loadGovernance();
    if (lastDetail) renderDetail(lastDetail);
  } else {
    loadChunks();
    if (lastDetail) renderDetail(lastDetail);
  }
}

// ── Theme ─────────────────────────────────────────────────────────────────
let THEME = localStorage.getItem("innate-theme");
if (THEME !== "light" && THEME !== "dark") THEME = "dark";

function applyTheme() {
  document.documentElement.setAttribute("data-theme", THEME);
  updateToggles();
}
function setTheme(theme) {
  if (theme === THEME) return;
  THEME = theme;
  localStorage.setItem("innate-theme", theme);
  applyTheme();
}

// ── Networking ──────────────────────────────────────────────────────────────
async function api(path, opts) {
  // Always attach the token header. On a loopback bind the server ignores it
  // for reads; on a network-exposed bind it is required for every endpoint.
  const o = opts ? { ...opts } : {};
  o.headers = { ...(o.headers || {}), "X-Innate-Token": TOKEN };
  const res = await fetch(path, o);
  const text = await res.text();
  let data = {};
  try { data = text ? JSON.parse(text) : {}; } catch (_) { data = { error: text }; }
  if (!res.ok) throw new Error(data.error || res.statusText);
  return data;
}

// ── Toasts (status disc + message) ───────────────────────────────────────────
function toast(msg, kind) {
  const wrap = $("toast-wrap");
  const el = document.createElement("div");
  el.className = "toast " + (kind === "err" ? "err" : "");
  const icon = document.createElement("span");
  icon.className = "toast-icon";
  icon.textContent = kind === "err" ? "!" : "✓";
  el.appendChild(icon);
  el.appendChild(document.createTextNode(msg));
  wrap.appendChild(el);
  setTimeout(() => { el.remove(); }, 3200);
}

// ── Time / format helpers ─────────────────────────────────────────────────
// Days between an ISO timestamp and now, floored. Returns null on bad input.
function ageDays(iso) {
  if (!iso) return null;
  const ts = Date.parse(iso);
  if (Number.isNaN(ts)) return null;
  return Math.max(0, Math.floor((Date.now() - ts) / 86400000));
}

// Render an ISO-8601 UTC timestamp as a local "YYYY-MM-DD HH:MM:SS" string.
function fmtTime(iso) {
  if (!iso) return "—";
  const ts = Date.parse(iso);
  if (Number.isNaN(ts)) return iso;
  const d = new Date(ts);
  const p = (n) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ` +
         `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

function fmtNum(n) {
  n = Number(n) || 0;
  return n >= 1000 ? (n / 1000).toFixed(n >= 10000 ? 0 : 1) + "k" : String(n);
}

function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, (m) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[m]));
}

function onTraces() { return !$("trace-view").classList.contains("hidden"); }

// Color by health, shared by the signal meters.
function healthColor(level) {
  return level === "ok" ? "var(--ok)" : level === "warn" ? "var(--warn)" : level === "muted" ? "var(--muted)" : "var(--danger)";
}

// ── Health strip + overview dashboard ───────────────────────────────────────
async function loadHealth() {
  try {
    const h = await api("/api/inspect");
    lastInspect = h;
    const chunks = h.chunks || {};
    const total = chunks.total ?? "—";
    const pending = chunks.pending ?? 0;
    const debt = h.knowledge_debt_ratio;
    const govPending = h.feedback_loop?.pending_governance_proposals ?? 0;
    const oldest = ageDays(chunks.pending_oldest_ts);
    lastGovPending = govPending;

    let html = `<span class="hgroup"><span class="hlabel">${t("health_chunks")}</span><b>${escapeHtml(String(total))}</b></span>`;
    if (debt != null) {
      html += `<span class="hsep"></span><span class="hgroup"><span class="hlabel">${t("health_debt")}</span><b>${Number(debt).toFixed(2)}</b></span>`;
    }
    const ageTxt = (oldest != null && pending > 0) ? `<span class="hage">(${t("health_oldest", oldest)})</span>` : "";
    html += `<span class="hsep"></span><span class="hgroup ${pending > 0 ? "hwarn" : ""}">` +
            `<span class="hlabel">${t("health_pending")}</span><b>${pending}</b>${ageTxt}</span>`;
    html += `<span class="hchevron">${overviewOpen ? "▴" : "▾"}</span>`;
    $("health").innerHTML = html;

    $("review-count").textContent = govPending;
    updateReviewBtn();
    if (overviewOpen) renderOverview(h);
  } catch (e) {
    $("health").textContent = t("inspect_failed") + ": " + e.message;
  }
}

function updateReviewBtn() {
  const b = $("review-btn");
  b.classList.toggle("on", govMode);
  b.classList.toggle("warn", !govMode && lastGovPending > 0);
  const dot = b.querySelector(".review-dot");
  if (dot) dot.classList.toggle("pulse", lastGovPending > 0);
}

function toggleOverview() {
  overviewOpen = !overviewOpen;
  $("overview").classList.toggle("hidden", !overviewOpen);
  $("overview").setAttribute("aria-hidden", String(!overviewOpen));
  $("health").setAttribute("aria-expanded", String(overviewOpen));
  const ch = $("health").querySelector(".hchevron");
  if (ch) ch.textContent = overviewOpen ? "▴" : "▾";
  if (overviewOpen) { if (lastInspect) renderOverview(lastInspect); else loadHealth(); }
}

function renderOverview(h) {
  const chunks = h.chunks || {};
  const total = Number(chunks.total ?? 0);
  const active = Number(chunks.active ?? 0);
  const pending = Number(chunks.pending ?? 0);
  const archived = Number(chunks.archived ?? 0);
  const invalidated = Math.max(0, total - active - pending - archived);
  const segs = [
    ["active", active, "var(--ok)"],
    ["pending", pending, "var(--warn)"],
    ["archived", archived, "var(--muted)"],
    ["invalidated", invalidated, "var(--danger)"],
  ];
  const segTotal = (active + pending + archived + invalidated) || 1;

  const debt = h.knowledge_debt_ratio;
  const rebuild = h.embed_rebuild_queue ?? 0;
  const cost = h.distill_cost_estimate || {};
  const costTokens = (Number(cost.prompt_tokens || 0) + Number(cost.completion_tokens || 0));
  const newLogs = h.episodic_log?.new ?? 0;

  const stat = (label, value, sub, cls) =>
    `<div class="ov-card stat"><span class="ov-eyebrow">${escapeHtml(label)}</span>` +
    `<span class="ov-val ${cls || ""}">${escapeHtml(value)}</span>` +
    `<span class="ov-sub">${escapeHtml(sub)}</span></div>`;

  const debtCls = (debt != null && Number(debt) >= 0.2) ? "warn" : "";
  const ov = $("overview");
  ov.innerHTML =
    `<div class="overview-grid">` +
      `<div class="ov-card">` +
        `<div class="ov-card-head"><span class="ov-eyebrow">${t("ov_composition")}</span><span class="ov-big">${total}</span></div>` +
        `<div class="ov-bar">` +
          segs.map(([k, n, c]) => `<div class="ov-bar-fill" data-w="${(n / segTotal * 100).toFixed(1)}" data-bg="${c}" title="${escapeHtml(t("st_" + k))}"></div>`).join("") +
        `</div>` +
        `<div class="ov-legend">` +
          segs.map(([k, n, c]) => `<span class="ov-legend-item"><span class="ov-legend-dot" data-bg="${c}"></span>${escapeHtml(t("st_" + k))} <b>${n}</b></span>`).join("") +
        `</div>` +
      `</div>` +
      stat(t("ov_debt"), debt != null ? Number(debt).toFixed(2) : "—", t("ov_debt_sub"), debtCls) +
      stat(t("ov_rebuild"), String(rebuild), t("ov_rebuild_sub"), "") +
      stat(t("ov_distill"), costTokens ? fmtNum(costTokens) + " tok" : "—", t("ov_distill_sub", newLogs), "ok") +
    `</div>` +
    renderObservability(h, stat);
  // CSP-safe: geometry/colour applied via the CSSOM (not inline style attributes).
  ov.querySelectorAll(".ov-bar-fill").forEach((e) => { e.style.width = e.dataset.w + "%"; e.style.background = e.dataset.bg; });
  ov.querySelectorAll(".ov-legend-dot").forEach((e) => { e.style.background = e.dataset.bg; });
}

// P4 observability panel: runtime / recall-quality stats from the new inspect blocks
// plus a prioritized anomaly list. Function over chart polish (design doc §6 P4).
function renderObservability(h, stat) {
  const obs = h.observability || {};
  const op = h.operational || {};
  const daemon = op.daemon || {};
  const ops = op.ops || {};
  const byOp = ops.by_op || {};
  const w7 = (obs.windows && obs.windows["7d"]) || {};
  const rp = obs.recall_pack || {};

  const pct = (x) => (x == null ? "—" : (Number(x) * 100).toFixed(0) + "%");
  const ms = (x) => (x == null ? "—" : fmtNum(x) + " ms");

  const runtime =
    `<div class="overview-grid">` +
      stat("daemon", daemon.state || "—",
           `${t("obs_errors_24h")}: ${daemon.errors_24h ?? 0}`,
           (Number(daemon.errors_24h) > 0) ? "warn" : "ok") +
      stat("recall p95", ms(byOp.recall?.p95_ms), t("obs_embed_p95", ms(byOp.embed?.p95_ms)), "") +
      stat(t("obs_empty_recall"), pct(w7.empty_recall_rate),
           `${t("obs_task_success")}: ${pct(w7.task_success_rate)}`,
           (Number(w7.empty_recall_rate) >= 0.3) ? "warn" : "ok") +
      stat(t("obs_mrr"), (rp.used_rank_mrr ?? "—"),
           `${t("obs_zombie")}: ${rp.zombie_chunks ?? 0}`, "") +
    `</div>`;

  // Anomaly list — prioritized, only shows what actually needs attention.
  const a = [];
  if (Number(rp.zombie_chunks) > 0) a.push(t("anom_zombie", rp.zombie_chunks));
  if (Number(w7.empty_recall_rate) >= 0.3) a.push(t("anom_empty", pct(w7.empty_recall_rate)));
  if (Number(daemon.errors_24h) > 0) a.push(t("anom_daemon", daemon.errors_24h));
  (ops.error_kind_top || []).slice(0, 3).forEach((e) => a.push(`${e.count}× ${e.error_kind}`));
  // slow ops (p95 > 5s)
  Object.entries(byOp).forEach(([name, s]) => {
    if (Number(s.p95_ms) > 5000) a.push(t("anom_slow", name, fmtNum(s.p95_ms)));
  });
  (h.suggestions || []).forEach((s) => { if (/INNATE_AGENT/.test(s.action || "")) a.push(s.reason); });

  const anomalies = a.length
    ? `<ul class="obs-anomalies">` + a.map((x) => `<li>⚠ ${escapeHtml(String(x))}</li>`).join("") + `</ul>`
    : `<div class="obs-clear">✓ ${escapeHtml(t("obs_no_anomalies"))}</div>`;

  return `<div class="obs-section"><div class="ov-eyebrow">${escapeHtml(t("obs_title"))}</div>` +
         runtime + anomalies + `</div>`;
}

// ── Review queue ────────────────────────────────────────────────────────────
function toggleReview() {
  if (onTraces()) {
    $("trace-view").classList.add("hidden");
    $("kb-view").classList.remove("hidden");
    $("tab-traces").classList.remove("active");
    $("tab-knowledge").classList.add("active");
  }
  govMode = !govMode;
  $("queue-banner").classList.toggle("hidden", !govMode);
  $("kb-filters").classList.toggle("hidden", govMode);
  updateReviewBtn();
  if (govMode) {
    loadGovernance();
  } else {
    offset = 0;
    loadChunks();
  }
}

async function loadGovernance() {
  try {
    const data = await api("/api/governance?state=pending");
    const proposals = data.proposals || [];
    renderGovernance(proposals);
    $("page-info").textContent = t("flagged", proposals.length);
    $("prev").disabled = true;
    $("next").disabled = true;
  } catch (e) {
    toast(t("queue_failed") + ": " + e.message, "err");
  }
}

function renderGovernance(proposals) {
  const ul = $("chunks");
  ul.innerHTML = "";
  currentChunkIds = proposals.map((p) => p.chunk_id);
  if (proposals.length === 0) {
    const li = document.createElement("li");
    li.className = "empty";
    li.textContent = t("gov_empty");
    ul.appendChild(li);
    return;
  }
  for (const p of proposals) {
    const li = document.createElement("li");
    li.dataset.id = p.chunk_id;
    if (p.chunk_id === selectedId) li.classList.add("sel");
    li.innerHTML =
      `<div class="row-head"><span class="row-skill">${escapeHtml(p.skill_name || "·")}</span>` +
      `<span class="row-seq">#${escapeHtml(String(p.seq ?? ""))}</span>` +
      `<span class="row-sub">${escapeHtml(p.proposal_type || "")}</span>` +
      `<span class="row-score">${t("score")} ${Number(p.evidence_score ?? 0).toFixed(1)} · ${t("actors", Number(p.actor_count ?? 0))}</span></div>` +
      `<div class="row-preview">${escapeHtml(p.content_preview || t("chunk_missing"))}</div>` +
      `<div class="row-reason"><span class="dot">●</span>${escapeHtml(p.reason || "")}</div>`;
    li.onclick = () => selectChunk(p.chunk_id);
    ul.appendChild(li);
  }
}

// ── Chunk list ──────────────────────────────────────────────────────────────
async function loadChunks() {
  const state = $("f-state").value;
  const origin = $("f-origin").value;
  const q = new URLSearchParams({ limit: LIMIT, offset });
  if (state) q.set("state", state);
  if (origin) q.set("origin", origin);
  try {
    const data = await api("/api/chunks?" + q.toString());
    const chunks = data.chunks || [];
    renderList(chunks);
    lastCount = chunks.length;
    updatePager();
  } catch (e) {
    toast(t("load_failed") + ": " + e.message, "err");
  }
}

// The API returns no total count, so "is there a next page?" is inferred: a full
// page (exactly LIMIT rows) may have more; a short/empty page is the last one.
function updatePager() {
  $("prev").disabled = offset === 0;
  $("next").disabled = lastCount < LIMIT;
  if (lastCount === 0) {
    $("page-info").textContent = offset > 0 ? t("no_more") : t("no_results");
  } else {
    $("page-info").textContent = `${offset + 1}–${offset + lastCount}`;
  }
}

function badge(state) {
  const s = state || "";
  return `<span class="badge ${escapeHtml(s)}">${escapeHtml(s) || "?"}</span>`;
}

function renderList(chunks) {
  const ul = $("chunks");
  ul.innerHTML = "";
  currentChunkIds = chunks.map((c) => c.id);
  if (chunks.length === 0) {
    const li = document.createElement("li");
    li.className = "empty";
    li.textContent = t("no_results");
    ul.appendChild(li);
    return;
  }
  for (const c of chunks) {
    const li = document.createElement("li");
    li.dataset.id = c.id;
    if (c.id === selectedId) li.classList.add("sel");
    li.innerHTML =
      `<div class="row-head"><span class="row-skill">${escapeHtml(c.skill_name || "·")}</span>` +
      `<span class="row-seq">#${escapeHtml(String(c.seq ?? ""))}</span>` +
      `<span class="row-sub">${escapeHtml(c.origin || "")}</span>${badge(c.state)}</div>` +
      `<div class="row-preview">${escapeHtml(c.content_preview || "")}</div>`;
    li.onclick = () => selectChunk(c.id);
    ul.appendChild(li);
  }
}

function markSelected(listSel, id) {
  document.querySelectorAll(listSel + " li").forEach((li) => {
    li.classList.toggle("sel", li.dataset.id === id);
  });
}

async function selectChunk(id) {
  selectedId = id;
  showRaw = false;
  markSelected("#chunks", id);
  try {
    const d = await api("/api/chunk/" + encodeURIComponent(id));
    renderDetail(d);
  } catch (e) {
    const el = $("detail");
    el.className = "detail-empty";
    el.textContent = t("detail_failed") + ": " + e.message;
  }
}

// Governance actions available for a chunk, by state.
function actionsFor(c) {
  const out = [];
  if (c.state === "pending") out.push({ label: t("act_approve"), cls: "filled-ok", kbd: "A", act: "approve" });
  if (c.state === "archived" || c.state === "invalidated") out.push({ label: t("act_restore"), cls: "filled-accent", act: "restore" });
  if (c.state === "active" || c.state === "pending") {
    out.push({ label: t("act_archive"), cls: "ghost-warn", act: "archive" });
    out.push({ label: t("act_invalidate"), cls: "ghost-danger", act: "invalidate" });
  }
  return out;
}

function renderDetail(d) {
  lastDetail = d;
  const c = d.chunk || d;
  selChunkState = c.state;
  const el = $("detail");
  el.className = "detail";

  const acts = actionsFor(c);

  // Signal meters: the two most telling signals as live gauges.
  const conf = c.confidence != null ? Number(c.confidence) : null;
  const used = Number(c.used_count ?? 0);
  const selected = Number(c.selected_count ?? 0);
  const rate = used > 0 ? selected / used : 0;
  const confLevel = conf == null ? "muted" : conf >= 0.7 ? "ok" : conf >= 0.45 ? "warn" : "danger";
  const hitLevel = used === 0 ? "muted" : rate >= 0.6 ? "ok" : rate >= 0.3 ? "warn" : "danger";
  const confColor = healthColor(confLevel);
  const hitColor = healthColor(hitLevel);
  const confVal = conf == null ? "—" : conf.toFixed(2);
  const confW = conf == null ? "0%" : Math.round(conf * 100) + "%";
  const hitVal = used > 0 ? Math.round(rate * 100) + "%" : "—";
  const hitW = used > 0 ? Math.round(rate * 100) + "%" : "0%";

  // KV grid slimmed to identity facts — confidence + usage now live as meters.
  const kvRows = [
    [t("k_id"), c.id],
    [t("k_origin"), c.origin],
    [t("k_created"), fmtTime(c.created_at)],
    [t("k_last_used"), fmtTime(c.last_used_at)],
  ];

  el.innerHTML =
    `<div class="detail-title"><h1>${escapeHtml(c.skill_name || "")} <span class="seq">#${escapeHtml(String(c.seq ?? ""))}</span></h1>${badge(c.state)}</div>` +
    `<div class="actions"><span class="actions-label">${t("governance")}</span>` +
      acts.map((a) => `<button class="act ${a.cls}" data-act="${a.act}">${escapeHtml(a.label)}${a.kbd ? `<span class="kbd">${a.kbd}</span>` : ""}</button>`).join("") +
    `</div>` +
    `<div class="signals">` +
      `<div class="meter"><div class="meter-head"><span class="meter-label">${t("k_confidence")}</span>` +
        `<span class="meter-val" data-fg="${confColor}">${confVal}</span></div>` +
        `<div class="meter-track"><div class="meter-fill" data-w="${confW}" data-bg="${confColor}"></div></div></div>` +
      `<div class="meter"><div class="meter-head"><span class="meter-label">${t("hit_rate")}</span>` +
        `<span class="meter-figs"><span class="meter-sub">${selected} / ${used}</span><span class="meter-val" data-fg="${hitColor}">${hitVal}</span></span></div>` +
        `<div class="meter-track"><div class="meter-fill" data-w="${hitW}" data-bg="${hitColor}"></div></div></div>` +
    `</div>` +
    `<div class="kv">` +
      kvRows.map(([k, v]) => `<div class="k">${escapeHtml(k)}</div><div class="v">${escapeHtml(String(v ?? "—"))}</div>`).join("") +
    `</div>` +
    `<div class="section"><div class="section-head"><span class="section-eyebrow">${t("h_content")}</span><span class="section-rule"></span></div>` +
      `<pre class="block">${escapeHtml(c.content || "")}</pre></div>` +
    `<div class="section"><button class="raw-toggle" id="raw-toggle"><span class="chev">${showRaw ? "▾" : "▸"}</span>${showRaw ? t("hide_raw") : t("show_raw")}</button>` +
      (showRaw ? `<pre class="raw-json">${escapeHtml(JSON.stringify(d, null, 2))}</pre>` : "") +
    `</div>`;

  // CSP-safe: meter geometry + colour via the CSSOM. A reflow read between the
  // 0-width markup and the real width lets the fill animate its sweep.
  const fills = el.querySelectorAll(".meter-fill");
  void el.offsetWidth;
  fills.forEach((e) => { e.style.background = e.dataset.bg; e.style.width = e.dataset.w; });
  el.querySelectorAll(".meter-val").forEach((e) => { e.style.color = e.dataset.fg; });

  el.querySelectorAll("button[data-act]").forEach((b) => { b.onclick = () => govern(c.id, b.dataset.act); });
  const rt = $("raw-toggle");
  if (rt) rt.onclick = () => { showRaw = !showRaw; renderDetail(lastDetail); };
}

// approve / restore act immediately; archive / invalidate open the reason sheet.
function govern(id, action) {
  if (action === "archive" || action === "invalidate") {
    openDialog(id, action);
  } else {
    postGovern(id, action, "");
  }
}

async function postGovern(id, action, reason) {
  const label = t("act_" + action) || action;
  try {
    await api("/api/chunk/" + encodeURIComponent(id) + "/" + action, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ reason }),
    });
    toast(t("action_ok", label), "ok");
    await selectChunk(id);
    if (govMode) loadGovernance(); else loadChunks();
    loadHealth();
  } catch (e) {
    toast(t("action_failed", label, e.message), "err");
  }
}

// ── Archive / invalidate dialog (replaces native prompt()/confirm()) ─────────
function openDialog(id, action) {
  dialog = { id, action };
  const isArch = action === "archive";
  $("dialog-title").textContent = isArch ? t("confirm_archive") : t("confirm_invalidate");
  $("dialog-desc").textContent = isArch ? t("archive_desc") : t("invalidate_desc");
  const confirm = $("dialog-confirm");
  confirm.textContent = isArch ? t("confirm_archive") : t("confirm_invalidate");
  confirm.className = "btn-primary " + (isArch ? "warn" : "danger");
  confirm.disabled = true;
  const icon = $("dialog-icon");
  icon.textContent = isArch ? "▤" : "✕";
  icon.className = "sheet-icon " + (isArch ? "warn" : "danger");
  const ta = $("dialog-reason");
  ta.value = "";
  ta.placeholder = t("reason_placeholder");
  $("dialog-overlay").classList.remove("hidden");
  ta.focus();
}

function closeDialog() {
  dialog = null;
  $("dialog-overlay").classList.add("hidden");
  $("dialog-reason").value = "";
}

function confirmDialog() {
  if (!dialog) return;
  const reason = $("dialog-reason").value.trim();
  if (!reason) return;
  const { id, action } = dialog;
  closeDialog();
  postGovern(id, action, reason);
}

// ── View switching ───────────────────────────────────────────────────────────
// Data-driven so adding a tab is a single map entry. Each view owns one <main>
// element, one tab button, and an optional loader run on activation.
const VIEWS = {
  knowledge: { el: "kb-view", tab: "tab-knowledge", load: () => loadChunks() },
  search: { el: "search-view", tab: "tab-search", load: null },
  play: { el: "play-view", tab: "tab-play", load: null },
  sessions: { el: "sessions-view", tab: "tab-sessions", load: () => loadSessions() },
  daemon: { el: "daemon-view", tab: "tab-daemon", load: () => loadDaemon() },
  traces: { el: "trace-view", tab: "tab-traces", load: () => loadTraces() },
};

function showView(which) {
  if (which !== "knowledge" && govMode) {
    govMode = false;
    $("queue-banner").classList.add("hidden");
    $("kb-filters").classList.remove("hidden");
    updateReviewBtn();
  }
  Object.entries(VIEWS).forEach(([key, v]) => {
    $(v.el).classList.toggle("hidden", key !== which);
    $(v.tab).classList.toggle("active", key === which);
  });
  const v = VIEWS[which];
  if (v && v.load) v.load();
}

// ── Search view (R3) ──────────────────────────────────────────────────────────
async function loadSearch() {
  const q = $("search-q").value.trim();
  if (!q) { $("search-results").innerHTML = ""; $("search-info").textContent = ""; return; }
  try {
    const data = await api("/api/search?" + new URLSearchParams({ q, limit: 50 }));
    const results = data.results || [];
    const ul = $("search-results");
    ul.innerHTML = "";
    $("search-info").textContent = results.length ? t("calls", results.length) : t("search_empty");
    results.forEach((r) => {
      const c = r.chunk || {};
      const li = document.createElement("li");
      li.className = "row";
      li.dataset.id = c.id || "";
      li.innerHTML = `<div class="row-main"><span class="row-title">${escapeHtml((c.content || "").slice(0, 90))}</span></div>` +
        `<div class="row-meta">${badge(c.state)}<span class="muted">${t("k_fused")}: ${r.score}</span></div>`;
      li.onclick = () => { showView("knowledge"); selectChunk(c.id); };
      ul.appendChild(li);
    });
  } catch (e) {
    toast(t("search_failed") + ": " + e.message, "err");
  }
}

// ── Recall Playground view (R6) ───────────────────────────────────────────────
async function loadPlayground() {
  const q = $("play-q").value.trim();
  if (!q) { $("play-results").innerHTML = ""; $("play-info").textContent = ""; return; }
  const params = new URLSearchParams({ q, budget: $("play-budget").value || 6000 });
  if ($("play-sparks").checked) params.set("include_sparks", "true");
  try {
    const data = await api("/api/playground?" + params);
    const items = (data.knowledge || []).concat(data.sparks || []);
    const ul = $("play-results");
    ul.innerHTML = "";
    $("play-info").textContent = items.length ? t("calls", items.length) : t("play_empty");
    items.forEach((c) => {
      const li = document.createElement("li");
      li.className = "row";
      li.dataset.id = c.id || "";
      const fused = c._fused_score != null ? c._fused_score.toFixed(3) : "—";
      li.innerHTML = `<div class="row-main"><span class="row-title">${escapeHtml((c.content || "").slice(0, 90))}</span></div>` +
        `<div class="row-meta">${badge(c.state)}<span class="muted">${t("k_fused")}: ${fused}</span></div>`;
      li.onclick = () => renderPlayDetail(c);
      ul.appendChild(li);
    });
  } catch (e) {
    toast(t("play_failed") + ": " + e.message, "err");
  }
}

function renderPlayDetail(c) {
  const el = $("play-detail");
  el.className = "detail";
  el.innerHTML = `<div class="detail-title"><h1>${escapeHtml(c.id || "")}</h1></div>` +
    `<pre class="block code">${escapeHtml(JSON.stringify(c, null, 2))}</pre>`;
}

// ── Sessions view (R8) ────────────────────────────────────────────────────────
async function loadSessions() {
  try {
    const data = await api("/api/sessions?limit=100");
    const rows = data.sessions || [];
    const ul = $("sessions-list");
    ul.innerHTML = "";
    $("sessions-info").textContent = rows.length ? t("calls", rows.length) : t("sessions_empty");
    rows.forEach((s) => {
      const li = document.createElement("li");
      li.className = "row";
      li.innerHTML = `<div class="row-main"><span class="row-title">${escapeHtml((s.query || "(no query)").slice(0, 80))}</span></div>` +
        `<div class="row-meta"><span class="muted">${escapeHtml(fmtTime(s.ts))}</span>` +
        `<span class="muted">${escapeHtml(s.event_source || "")}${s.outcome ? " · " + escapeHtml(s.outcome) : ""}</span></div>`;
      li.onclick = () => {
        const d = $("sessions-detail");
        d.className = "detail";
        d.innerHTML = `<pre class="block code">${escapeHtml(JSON.stringify(s, null, 2))}</pre>`;
      };
      ul.appendChild(li);
    });
  } catch (e) {
    toast(t("sessions_failed") + ": " + e.message, "err");
  }
}

// ── Daemon status view (R7) ───────────────────────────────────────────────────
async function loadDaemon() {
  try {
    const h = await api("/api/daemon");
    const kv = (k, v) => `<div class="k">${escapeHtml(k)}</div><div class="v">${escapeHtml(String(v ?? "—"))}</div>`;
    let html = `<div class="kv">` +
      kv("state", h.state) + kv("running", h.running) + kv("pid", h.pid) +
      kv("processed_events", h.processed_events) +
      kv("errors_24h", h.errors_24h) + kv("errors_7d", h.errors_7d) +
      `</div>`;
    if (h.last_error) {
      html += `<pre class="block err">${escapeHtml(JSON.stringify(h.last_error, null, 2))}</pre>`;
    }
    if (h.watches && h.watches.length) {
      html += `<pre class="block code">${escapeHtml(JSON.stringify(h.watches, null, 2))}</pre>`;
    }
    $("daemon-card").innerHTML = html;
  } catch (e) {
    toast(t("daemon_failed") + ": " + e.message, "err");
  }
}

async function loadTraces() {
  const kind = $("t-kind").value;
  const status = $("t-status").value;
  const q = new URLSearchParams({ limit: 300 });
  if (kind) q.set("kind", kind);
  if (status) q.set("status", status);
  try {
    const data = await api("/api/llm-traces?" + q.toString());
    renderTraces(data.traces || []);
  } catch (e) {
    toast(t("traces_failed") + ": " + e.message, "err");
  }
}

function traceBadge(status) {
  return `<span class="badge ${status === "ok" ? "active" : "warn"}">${escapeHtml(status || "?")}</span>`;
}

function renderTraces(traces) {
  const ul = $("traces");
  ul.innerHTML = "";
  currentTraces = traces;
  $("trace-info").textContent = t("calls", traces.length);
  if (traces.length === 0) {
    selectedTraceIdx = -1;
    const li = document.createElement("li");
    li.className = "empty";
    li.textContent = t("trace_empty");
    ul.appendChild(li);
    return;
  }
  traces.forEach((tr, idx) => {
    const li = document.createElement("li");
    li.dataset.idx = String(idx);
    if (idx === selectedTraceIdx) li.classList.add("sel");
    const tok = tr.token_usage && tr.token_usage.total_tokens != null
      ? ` · ${t("tok", tr.token_usage.total_tokens)}` : "";
    li.innerHTML =
      `<div class="row-head"><span class="row-skill">${escapeHtml(tr.kind || "?")}</span>` +
      `<span class="row-model">${escapeHtml(tr.model || "")}</span>${traceBadge(tr.status)}</div>` +
      `<div class="row-meta">${escapeHtml(fmtTime(tr.ts))} · ${tr.latency_ms ?? "?"}ms · ${t("tries", tr.attempts ?? 1)}${tok}</div>`;
    li.onclick = () => selectTrace(idx);
    ul.appendChild(li);
  });
}

function selectTrace(idx) {
  selectedTraceIdx = idx;
  document.querySelectorAll("#traces li").forEach((li) => {
    li.classList.toggle("sel", li.dataset.idx === String(idx));
  });
  renderTraceDetail(currentTraces[idx]);
}

function renderTraceDetail(tr) {
  if (!tr) return;
  const el = $("trace-detail");
  el.className = "detail";
  const pretty = (s) => { try { return JSON.stringify(JSON.parse(s), null, 2); } catch (_) { return s; } };
  const kvRows = [
    [t("k_time"), fmtTime(tr.ts)],
    [t("k_model"), tr.model],
    [t("k_host"), tr.host],
    [t("k_latency"), (tr.latency_ms ?? "?") + " ms"],
    [t("k_attempts"), tr.attempts],
    [t("k_tokens"), tr.token_usage ? JSON.stringify(tr.token_usage) : "—"],
  ];
  const section = (eyebrow, body, cls) =>
    `<div class="section"><div class="section-head"><span class="section-eyebrow ${cls || ""}">${escapeHtml(eyebrow)}</span><span class="section-rule"></span></div>${body}</div>`;

  el.innerHTML =
    `<div class="detail-title"><h1>${escapeHtml(tr.kind || "")}</h1>${traceBadge(tr.status)}` +
      `<span class="spacer">${escapeHtml(fmtTime(tr.ts))}</span></div>` +
    `<div class="kv">` +
      kvRows.map(([k, v]) => `<div class="k">${escapeHtml(k)}</div><div class="v">${escapeHtml(String(v ?? "—"))}</div>`).join("") +
    `</div>` +
    (tr.error ? section(t("h_error"), `<pre class="block err">${escapeHtml(String(tr.error))}</pre>`, "danger") : "") +
    section(t("h_request"), `<pre class="block code">${escapeHtml(pretty(tr.request_preview || ""))}</pre>`) +
    section(t("h_response"), `<pre class="block code">${escapeHtml(pretty(tr.response_preview || "") || t("none"))}</pre>`);
}

// ── Keyboard ────────────────────────────────────────────────────────────────
// ↑/↓ navigate the active list; Esc closes the dialog; A approves a pending chunk.
function onKey(e) {
  if (e.key === "Escape" && dialog) { closeDialog(); return; }
  if (dialog) return;
  const tag = (e.target && e.target.tagName) || "";
  if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return;

  if ((e.key === "a" || e.key === "A") && !onTraces() && selectedId && selChunkState === "pending") {
    e.preventDefault();
    govern(selectedId, "approve");
    return;
  }
  if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
  e.preventDefault();
  const dir = e.key === "ArrowDown" ? 1 : -1;
  if (onTraces()) {
    if (!currentTraces.length) return;
    const i = selectedTraceIdx < 0 ? 0 : selectedTraceIdx + dir;
    selectTrace(Math.max(0, Math.min(currentTraces.length - 1, i)));
  } else {
    if (!currentChunkIds.length) return;
    const cur = currentChunkIds.indexOf(selectedId);
    const i = cur < 0 ? 0 : cur + dir;
    selectChunk(currentChunkIds[Math.max(0, Math.min(currentChunkIds.length - 1, i))]);
  }
}

// ── Wiring ──────────────────────────────────────────────────────────────────
$("tab-knowledge").onclick = () => showView("knowledge");
$("tab-search").onclick = () => showView("search");
$("tab-play").onclick = () => showView("play");
$("tab-sessions").onclick = () => showView("sessions");
$("tab-daemon").onclick = () => showView("daemon");
$("tab-traces").onclick = () => showView("traces");

$("search-go").onclick = loadSearch;
$("search-q").addEventListener("keydown", (e) => { if (e.key === "Enter") loadSearch(); });
$("play-go").onclick = loadPlayground;
$("play-q").addEventListener("keydown", (e) => { if (e.key === "Enter") loadPlayground(); });
$("sessions-reload").onclick = loadSessions;
$("daemon-reload").onclick = loadDaemon;
$("health").onclick = toggleOverview;
$("review-btn").onclick = toggleReview;
$("queue-exit").onclick = toggleReview;
$("lang-toggle").onclick = () => setLang(LANG === "en" ? "zh" : "en");
$("theme-toggle").onclick = () => setTheme(THEME === "dark" ? "light" : "dark");

$("reload").onclick = () => { offset = 0; if (govMode) loadGovernance(); else loadChunks(); };
$("f-state").onchange = () => { offset = 0; loadChunks(); };
$("f-origin").onchange = () => { offset = 0; loadChunks(); };
$("prev").onclick = () => { if (!govMode && offset >= LIMIT) { offset -= LIMIT; loadChunks(); } };
$("next").onclick = () => { if (!govMode && lastCount === LIMIT) { offset += LIMIT; loadChunks(); } };

$("t-reload").onclick = loadTraces;
$("t-kind").onchange = loadTraces;
$("t-status").onchange = loadTraces;

$("dialog-cancel").onclick = closeDialog;
$("dialog-confirm").onclick = confirmDialog;
$("dialog-reason").oninput = () => { $("dialog-confirm").disabled = !$("dialog-reason").value.trim(); };
$("dialog-overlay").onclick = (e) => { if (e.target === $("dialog-overlay")) closeDialog(); };

window.addEventListener("keydown", onKey);

// ── Boot ────────────────────────────────────────────────────────────────────
applyTheme();
applyStatic();
loadHealth();
loadChunks();
