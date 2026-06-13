"use strict";

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

let offset = 0;
const LIMIT = 50;
let lastCount = 0; // rows returned by the most recent chunk page (drives the pager)
let selectedId = null;

const $ = (id) => document.getElementById(id);

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

function toast(msg, kind) {
  const t = $("toast");
  t.textContent = msg;
  t.className = "toast " + (kind || "");
  setTimeout(() => t.classList.add("hidden"), 3200);
}

// Days between an ISO timestamp and now, floored. Returns null on bad input.
function ageDays(iso) {
  if (!iso) return null;
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return null;
  return Math.max(0, Math.floor((Date.now() - t) / 86400000));
}

// Render an ISO-8601 UTC timestamp (e.g. 2026-06-13T12:47:52.506Z) as a local
// "YYYY-MM-DD HH:MM:SS" string — drops the millisecond / T / Z noise. Falls back
// to the raw string on unparseable input.
function fmtTime(iso) {
  if (!iso) return "—";
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return iso;
  const d = new Date(t);
  const p = (n) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ` +
         `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

async function loadHealth() {
  try {
    const h = await api("/api/inspect");
    // Field names must track the real inspect() shape (kb/inspection.rs):
    // chunks.{total,pending,pending_oldest_ts}, knowledge_debt_ratio,
    // feedback_loop.pending_governance_proposals.
    const chunks = h.chunks || {};
    const total = chunks.total ?? "—";
    const pending = chunks.pending ?? 0;
    const debt = h.knowledge_debt_ratio;
    const govPending = h.feedback_loop?.pending_governance_proposals ?? 0;
    const oldest = ageDays(chunks.pending_oldest_ts);

    let html =
      `chunks <b>${total}</b>` +
      (debt != null ? ` · debt <b>${Number(debt).toFixed(2)}</b>` : "");
    // Pending review backlog + how long the oldest item has waited (review SLA).
    const pendCls = pending > 0 ? "warn" : "";
    let pendTxt = `pending <b>${pending}</b>`;
    if (oldest != null && pending > 0) pendTxt += ` <span class="muted">(oldest ${oldest}d)</span>`;
    html += ` · <span class="${pendCls}">${pendTxt}</span>`;
    // Auto-flagged review queue (governance proposals) — click to inspect.
    const govCls = govPending > 0 ? "warn" : "";
    html += ` · <a href="#" id="gov-link" class="${govCls}">review queue <b>${govPending}</b></a>`;

    $("health").innerHTML = html;
    const link = $("gov-link");
    if (link) link.onclick = (e) => { e.preventDefault(); toggleGovernance(); };
  } catch (e) {
    $("health").textContent = "inspect failed: " + e.message;
  }
}

// Toggle between the normal chunk list and the flagged review queue.
let govMode = false;
async function toggleGovernance() {
  govMode = !govMode;
  $("queue-banner").classList.toggle("hidden", !govMode);
  if (govMode) {
    await loadGovernance();
  } else {
    offset = 0;
    loadChunks();
  }
}

async function loadGovernance() {
  try {
    const data = await api("/api/governance?state=pending");
    renderGovernance(data.proposals || []);
    $("page-info").textContent = `${(data.proposals || []).length} flagged`;
    // The review queue isn't paginated — neutralize the chunk pager.
    $("prev").disabled = true;
    $("next").disabled = true;
  } catch (e) {
    toast("review queue failed: " + e.message, "err");
  }
}

function renderGovernance(proposals) {
  const ul = $("chunks");
  ul.innerHTML = "";
  if (proposals.length === 0) {
    const li = document.createElement("li");
    li.className = "empty";
    li.textContent = "No chunks flagged for review.";
    ul.appendChild(li);
    return;
  }
  for (const p of proposals) {
    const li = document.createElement("li");
    if (p.chunk_id === selectedId) li.className = "sel";
    li.innerHTML =
      `<div class="chunk-head"><span>${escapeHtml(p.skill_name || "·")} #${escapeHtml(String(p.seq ?? ""))} ` +
      `<span class="muted">${escapeHtml(p.proposal_type || "")}</span></span>` +
      `<span class="badge warn">score ${Number(p.evidence_score ?? 0).toFixed(1)} · ${Number(p.actor_count ?? 0)} actors</span></div>` +
      `<div class="chunk-preview">${escapeHtml(p.content_preview || "(chunk missing)")}</div>` +
      `<div class="muted small">${escapeHtml(p.reason || "")}</div>`;
    li.onclick = () => selectChunk(p.chunk_id);
    ul.appendChild(li);
  }
}

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
    toast("load failed: " + e.message, "err");
  }
}

// The API returns no total count, so "is there a next page?" is inferred: a full
// page (exactly LIMIT rows) may have more; a short/empty page is the last one.
// This stops Next from paging forever into empty results.
function updatePager() {
  $("prev").disabled = offset === 0;
  $("next").disabled = lastCount < LIMIT;
  if (lastCount === 0) {
    $("page-info").textContent = offset > 0 ? "no more results" : "no results";
  } else {
    $("page-info").textContent = `${offset + 1}–${offset + lastCount}`;
  }
}

function badge(state) {
  const s = escapeHtml(state || "");
  return `<span class="badge ${s}">${s || "?"}</span>`;
}

function renderList(chunks) {
  const ul = $("chunks");
  ul.innerHTML = "";
  for (const c of chunks) {
    const li = document.createElement("li");
    if (c.id === selectedId) li.className = "sel";
    li.innerHTML =
      `<div class="chunk-head"><span>${escapeHtml(c.skill_name || "·")} #${escapeHtml(String(c.seq ?? ""))} ` +
      `<span class="muted">${escapeHtml(c.origin || "")}</span></span>${badge(c.state)}</div>` +
      `<div class="chunk-preview">${escapeHtml(c.content_preview || "")}</div>`;
    li.onclick = () => selectChunk(c.id);
    ul.appendChild(li);
  }
}

async function selectChunk(id) {
  selectedId = id;
  document.querySelectorAll(".chunks li").forEach((li) => li.classList.remove("sel"));
  try {
    const d = await api("/api/chunk/" + encodeURIComponent(id));
    renderDetail(d);
  } catch (e) {
    $("detail").textContent = "detail failed: " + e.message;
  }
  if (govMode) loadGovernance(); else loadChunks();
}

function renderDetail(d) {
  const c = d.chunk || d;
  const kv = (k, v) => `<div class="k">${k}</div><div>${escapeHtml(String(v ?? "—"))}</div>`;
  $("detail").classList.remove("muted");
  $("detail").innerHTML =
    `<h2>${escapeHtml(c.skill_name || "")} #${escapeHtml(String(c.seq ?? ""))} ${badge(c.state)}</h2>` +
    `<div class="actions">
       <button class="ok" data-act="approve">Approve</button>
       <button data-act="restore">Restore</button>
       <button class="danger" data-act="archive">Archive…</button>
       <button class="danger" data-act="invalidate">Invalidate…</button>
     </div>` +
    `<div class="kv">${kv("id", c.id)}${kv("origin", c.origin)}${kv("confidence", c.confidence)}` +
    `${kv("created", fmtTime(c.created_at))}${kv("last used", fmtTime(c.last_used_at))}` +
    `${kv("used / selected", (c.used_count ?? 0) + " / " + (c.selected_count ?? 0))}</div>` +
    `<h2>content</h2><pre>${escapeHtml(c.content || "")}</pre>` +
    `<h2>raw</h2><pre>${escapeHtml(JSON.stringify(d, null, 2))}</pre>`;
  $("detail").querySelectorAll("button[data-act]").forEach((b) => {
    b.onclick = () => govern(c.id, b.dataset.act);
  });
}

async function govern(id, action) {
  let reason = "";
  if (action === "archive" || action === "invalidate") {
    reason = prompt(`Reason for ${action}:`, "");
    if (reason == null || !reason.trim()) return; // cancelled / empty
  }
  if (!confirm(`${action} chunk ${id}?`)) return;
  try {
    await api("/api/chunk/" + encodeURIComponent(id) + "/" + action, {
      method: "POST",
      headers: { "Content-Type": "application/json", "X-Innate-Token": TOKEN },
      body: JSON.stringify({ reason }),
    });
    toast(`${action} ok`, "ok");
    selectChunk(id);
    loadHealth();
  } catch (e) {
    toast(`${action} failed: ${e.message}`, "err");
  }
}

function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, (m) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[m]));
}

// Leaving queue mode whenever the user drives the normal filters keeps the two
// views from desyncing (filters don't apply to the governance queue).
function exitGovMode() {
  if (!govMode) return;
  govMode = false;
  $("queue-banner").classList.add("hidden");
}
$("reload").onclick = () => { offset = 0; if (govMode) loadGovernance(); else loadChunks(); };
$("f-state").onchange = () => { exitGovMode(); offset = 0; loadChunks(); };
$("f-origin").onchange = () => { exitGovMode(); offset = 0; loadChunks(); };
$("prev").onclick = () => { if (!govMode && offset >= LIMIT) { offset -= LIMIT; loadChunks(); } };
$("next").onclick = () => { if (!govMode && lastCount === LIMIT) { offset += LIMIT; loadChunks(); } };

// ── LLM trace view ──────────────────────────────────────────────────────────
// A separate panel (not overloaded onto the chunk list) showing recent LLM /
// embedding HTTP calls for debugging what the agent actually sent and got back.

function showView(which) {
  const onTraces = which === "traces";
  $("kb-view").classList.toggle("hidden", onTraces);
  $("trace-view").classList.toggle("hidden", !onTraces);
  $("tab-knowledge").classList.toggle("active", !onTraces);
  $("tab-traces").classList.toggle("active", onTraces);
  if (onTraces) loadTraces();
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
    toast("traces failed: " + e.message, "err");
  }
}

function traceBadge(status) {
  const cls = status === "ok" ? "active" : "warn";
  return `<span class="badge ${cls}">${status || "?"}</span>`;
}

function renderTraces(traces) {
  const ul = $("traces");
  ul.innerHTML = "";
  $("trace-info").textContent = `${traces.length} calls`;
  if (traces.length === 0) {
    const li = document.createElement("li");
    li.className = "empty";
    li.textContent = "No LLM calls traced yet. Trigger a recall / evolve, then reload.";
    ul.appendChild(li);
    return;
  }
  traces.forEach((t, i) => {
    const li = document.createElement("li");
    const tok = t.token_usage && t.token_usage.total_tokens != null
      ? ` · ${t.token_usage.total_tokens} tok` : "";
    li.innerHTML =
      `<div class="chunk-head"><span>${t.kind || "?"} <span class="muted">${escapeHtml(t.model || "")}</span></span>` +
      `${traceBadge(t.status)}</div>` +
      `<div class="muted small">${escapeHtml(fmtTime(t.ts))} · ${t.latency_ms ?? "?"}ms · ${t.attempts ?? 1} try${tok}</div>`;
    li.onclick = () => {
      document.querySelectorAll("#traces li").forEach((x) => x.classList.remove("sel"));
      li.classList.add("sel");
      renderTraceDetail(t);
    };
    ul.appendChild(li);
  });
}

function renderTraceDetail(t) {
  const kv = (k, v) => `<div class="k">${k}</div><div>${escapeHtml(String(v ?? "—"))}</div>`;
  const d = $("trace-detail");
  d.classList.remove("muted");
  let pretty = (s) => { try { return JSON.stringify(JSON.parse(s), null, 2); } catch (_) { return s; } };
  d.innerHTML =
    `<h2>${t.kind} ${traceBadge(t.status)}</h2>` +
    `<div class="kv">${kv("time", fmtTime(t.ts))}${kv("model", t.model)}${kv("host", t.host)}` +
    `${kv("latency", (t.latency_ms ?? "?") + " ms")}${kv("attempts", t.attempts)}` +
    `${kv("tokens", t.token_usage ? JSON.stringify(t.token_usage) : "—")}</div>` +
    (t.error ? `<h2>error</h2><pre class="err-pre">${escapeHtml(String(t.error))}</pre>` : "") +
    `<h2>request</h2><pre>${escapeHtml(pretty(t.request_preview || ""))}</pre>` +
    `<h2>response</h2><pre>${escapeHtml(pretty(t.response_preview || "") || "(none)")}</pre>`;
}

$("tab-knowledge").onclick = () => showView("knowledge");
$("tab-traces").onclick = () => showView("traces");
$("t-reload").onclick = loadTraces;
$("t-kind").onchange = loadTraces;
$("t-status").onchange = loadTraces;

loadHealth();
loadChunks();
