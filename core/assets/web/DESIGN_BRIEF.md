# Innate Web UI — Design Brief

A handoff spec for redesigning `innate web`, the local HTTP UI for browsing and
governing the Innate knowledge base. This documents **what the page must do**;
the visual design is open to reinvention within the hard constraints below.

---

## 1. Product context

`innate web` is a **localhost governance console** for a personal "procedural
knowledge base" (an agent's long-term memory). A single user runs it on their own
machine to:

- **Browse** knowledge chunks (the atomic memory units) and inspect their detail.
- **Govern** chunks: approve pending ones, archive/invalidate bad ones, restore.
- **Triage** a review queue of chunks auto-flagged by negative feedback.
- **Debug** the LLM/embedding HTTP calls the system makes (latency, tokens, errors).
- **Monitor** overall KB health (counts, debt ratio, review backlog).

Audience: one technical power-user (the KB owner), not a public/marketing surface.
Tone target: a calm, dense, trustworthy *operator console* — closer to a database
admin tool or observability dashboard than a consumer app.

---

## 2. Hard constraints (must not break)

These are non-negotiable; the redesign lives entirely inside them.

- **Vanilla only.** Three static files served from the Rust binary via
  `include_str!`: `index.html`, `style.css`, `app.js`. **No build step, no
  framework, no npm, no external fonts/CDNs.** System font stack only.
- **Strict CSP.** `default-src 'none'; script-src 'self'; style-src 'self';
  connect-src 'self'; base-uri 'none'; form-action 'none'`. That means:
  **no inline `<script>`, no inline `style=""`, no inline event handlers, no
  external resources.** All JS in `app.js`, all CSS in `style.css`.
- **App-shell layout.** Page itself never scrolls (`body { height:100vh;
  overflow:hidden }`). Header fixed, left list pane fixed, only the inner regions
  scroll. **Do not reintroduce `position: sticky`** — it jitters on fractional-DPI
  displays (the reason the current build moved to a flex app-shell).
- **Security model preserved.** A token arrives in the URL *fragment*
  (`#token=…`), is read once into memory, and stripped from the address bar.
  Every API call sends it as `X-Innate-Token`. Don't log it, don't put it in the
  query string, don't persist it.
- **Keep theme + i18n.** Light/dark themes (CSS vars under
  `[data-theme="light|dark"]`, persisted in `localStorage["innate-theme"]`) and
  EN/中文 bilingual (`I18N` dict + `t()` + `data-i18n` attributes, persisted in
  `localStorage["innate-lang"]`). These already exist; redesign should refine,
  not remove.
- **Custom scrollbars** styled to the theme (WebKit + Firefox) should remain.

---

## 3. Information architecture

```
Header (always visible)
 ├─ Brand: "Innate" + subtitle "knowledge base"
 ├─ Tab switch:  [Knowledge] | [LLM Traces]
 ├─ Health bar (live metrics, see §5.1)
 └─ Toggles:  [lang 中/EN]  [theme 🌙/☀]

View A — Knowledge   (default)
 ├─ Left pane (fixed): filters + chunk list + pager
 └─ Right pane (scrolls): chunk detail + governance actions
   └─ sub-mode: Review Queue (replaces the list with flagged proposals)

View B — LLM Traces
 ├─ Left pane (fixed): filters + call list
 └─ Right pane (scrolls): call detail (request/response/error)
```

Two top-level views (tabs), and within View A a **Review-Queue mode** toggled
from the health bar.

---

## 4. Layout today (for reference, not prescriptive)

- Two-column split: **420px fixed sidebar** + fluid detail pane.
- Sidebar = filters bar (top) → scrolling list (middle) → pager (bottom).
- Detail pane = scrolling document of headings + key/value grids + `<pre>` blocks.
- Toast notifications: fixed, bottom-center, auto-dismiss ~3.2s.

The redesign may rethink proportions, density, and the list/detail relationship
(e.g. master-detail vs. drawer vs. modal) as long as §2 holds.

---

## 5. Feature & data inventory

All data comes from these read endpoints (JSON). Governance is POST.

### 5.1 Health bar — `GET /api/inspect`
Compact live status line. Fields used:
- `chunks.total` → "chunks N"
- `knowledge_debt_ratio` → "debt 0.NN" (optional)
- `chunks.pending` → "pending N" (warn color if >0)
- `chunks.pending_oldest_ts` → "(oldest Nd)" age of oldest pending item (review SLA)
- `feedback_loop.pending_governance_proposals` → "review queue N" — **clickable**,
  toggles Review-Queue mode (warn color if >0)

Design note: this is a status strip; it should read at a glance and signal
backlog/urgency. `inspect()` returns much more (debt, rebuild queue, episodic log
counts, distill cost estimate) — there's room for a richer **dashboard/overview**
if desired.

### 5.2 Chunk list — `GET /api/chunks?state=&origin=&limit=50&offset=`
Filters: **state** (all/pending/active/archived/invalidated), **origin**
(all/captured/distilled/spark). Reload button. Pager: Prev / "M–N" / Next.
- Pagination has **no total count** from the API. "Has next page" is inferred:
  a full page (exactly `LIMIT=50` rows) may have more; a short/empty page is last.
  Prev disabled at offset 0; Next disabled when page < LIMIT.
Each row shows: `skill_name`, `#seq`, `origin` (muted), a **state badge**, and a
2-line truncated `content_preview`. Selected row is highlighted.

### 5.3 Chunk detail — `GET /api/chunk/{id}`
Header: `skill_name #seq` + state badge. Then **governance actions** (see 5.5),
a key/value grid (`id`, `origin`, `confidence`, `created`, `last used`,
`used / selected` counts), the full `content` in a `<pre>`, and a raw JSON dump.
- Timestamps render as local `YYYY-MM-DD HH:MM:SS` (helper `fmtTime`), not raw ISO.

### 5.4 Review queue (governance proposals) — `GET /api/governance?state=pending`
Entered by clicking "review queue" in the health bar; shows a banner and replaces
the chunk list. Each proposal: `skill_name #seq`, `proposal_type`, a warn badge
"score X · N actors" (`evidence_score`, `actor_count`), `content_preview`, and a
`reason`. Filters/pager are disabled in this mode. Clicking a proposal opens the
normal chunk detail. Empty state: "No chunks flagged for review."

### 5.5 Governance actions — `POST /api/chunk/{id}/{action}`
Four actions on a chunk: **approve**, **restore**, **archive** (requires a
non-empty reason), **invalidate** (requires reason). Archive/invalidate currently
use browser `prompt()` for the reason and `confirm()` for confirmation — a prime
candidate for a proper in-page dialog in the redesign. On success: toast + refresh
detail + refresh health.

### 5.6 LLM Traces list — `GET /api/llm-traces?kind=&status=&limit=300`
Filters: **kind** (chat/embedding), **status** (ok/http_4xx/http_5xx/rate_limited/
transport/error). Each row: `kind`, `model` (muted), a status badge (ok=green,
else warn), and a meta line: `time · latency_ms ms · N try · M tok`. Newest first.
Count shown as "N calls". Empty state guides the user to trigger a recall/evolve.

### 5.7 Trace detail — (from the in-memory trace object)
Header: `kind` + status badge. KV grid: `time`, `model`, `host`, `latency`,
`attempts`, `tokens`. Optional **error** block (red `<pre>`). Then `request` and
`response` previews as pretty-printed JSON in `<pre>` blocks. **Never shows the
API key** (the server redacts it before logging).

---

## 6. States to design

- **Loading** (currently none — silent fetch). Consider subtle skeletons/spinners.
- **Empty**: no chunks, no proposals, no traces (each has a message today).
- **Error**: fetch failures surface as a toast or inline text.
- **Selected** vs unselected list rows; **disabled** pager/filter states.
- **Badges**: chunk state (pending=warn, active=ok, archived/invalidated=danger),
  trace status (ok vs warn). These semantic colors should survive the restyle.
- **Warn/urgency** signaling in the health bar (pending backlog, review queue).
- **Theme**: every surface must work in both light and dark.
- **Bilingual**: every label must come from the `I18N` dict (EN + 中文); leave
  technical enum values (e.g. `http_4xx`, `pending`) literal where they mirror data.

---

## 7. Known weak spots / design goals

Things worth improving in a redesign:

1. **Native `prompt()` / `confirm()`** for archive/invalidate reasons — replace
   with a styled, CSP-safe in-page dialog/sheet.
2. **Health bar is cramped** — it packs 5 metrics into one line; `inspect()` has
   far more (debt, rebuild queue, episodic counts, distill cost). An overview
   panel or expandable dashboard could surface KB health better.
3. **Raw JSON dump** in chunk detail is developer-grade; consider a cleaner
   structured view with raw JSON behind a toggle.
4. **Master-detail density** — the 420px sidebar + long scrolling detail is
   functional but visually flat. Hierarchy, spacing, and typographic rhythm are
   the main opportunity.
5. **No keyboard navigation** (arrow keys through the list, shortcuts for
   approve/archive) — a nice-to-have for an operator tool.
6. **Toast-only feedback** — fine, but could be richer (inline confirmation).
7. **Review-queue entry is hidden** behind a health-bar link — discoverability.

---

## 8. Deliverable expectations for the redesign

- Updated `index.html` + `style.css` + `app.js` that keep **all** behaviors in §5,
  all constraints in §2, and the state matrix in §6.
- Both themes polished; both languages complete (extend the `I18N` dict for any
  new strings — every user-visible string goes through `t()` or `data-i18n`).
- No regressions to: token handling, pagination inference, app-shell scrolling,
  custom scrollbars, governance flows.
- Verify by running `innate web` and exercising: browse/filter/paginate, open a
  chunk, run each governance action, toggle review queue, switch to traces and
  open a call, toggle theme, toggle language.

---

## Appendix A — Current wireframes (ASCII)

Rough as-built layout of each screen. Boxes are panes; `▓` marks the selected
row; `░` a hover/other row. This is the *current* structure to redesign, not a
target.

### Header (always visible, full width, fixed)

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ Innate  knowledge base   [ Knowledge ][ LLM Traces ]                           │
│                                  chunks 128 · debt 0.12 · pending 4 (oldest 6d) │
│                                  · review queue 2          [ 中 ] [ 🌙 ]        │
└──────────────────────────────────────────────────────────────────────────────┘
   brand+subtitle    tab switch         health bar (live, warn-colored)   lang/theme
```

### View A — Knowledge (default): fixed 420px list  +  scrolling detail

```
┌──────────── list pane (fixed) ───────────┬──────── detail pane (scrolls) ────────┐
│ [ state: all ▾ ] [ origin: all ▾ ] [Reload]│  build #0                  [ active ] │
├───────────────────────────────────────────┤  ┌─ actions ─────────────────────────┐│
│ build #0    captured          [pending] ░ │  │ [Approve][Restore][Archive…][Inval…]││
│   用 cargo build --release 构建 innate 二… │  └───────────────────────────────────┘│
│ ───────────────────────────────────────── │  id              38af2cde-67b2-…       │
│ recall #3   distilled          [active] ▓ │  origin          captured              │
│   召回时写入 usage_trace(retrieved/sele…   │  confidence      0.60                  │
│ ───────────────────────────────────────── │  created         2026-06-13 12:37:18   │
│ evolve #7   distilled         [archived]   │  last used       —                     │
│   evolve 蒸馏 new→pending chunks …         │  used / selected 0 / 0                 │
│                                            │                                        │
│        ⋮  (list scrolls inside pane)       │  content                               │
│                                            │  ┌──────────────────────────────────┐  │
│                                            │  │ ...full chunk text (wrapped pre)…│  │
│                                            │  └──────────────────────────────────┘  │
├───────────────────────────────────────────┤  raw                                   │
│ [ ‹ Prev ]        1–50         [ Next › ]  │  ┌──────────────────────────────────┐  │
│  (disabled)                                │  │ { "id": …, "state": "active", … }│  │
└───────────────────────────────────────────┴──┴──────────────────────────────────┴──┘
   filters → scrolling list → pager              headings + KV grid + <pre> blocks
```

### View A — Review-Queue mode (list replaced; entered from health bar)

```
┌──────────── list pane (fixed) ───────────┐
│ ⚠ Review queue — chunks flagged by repeat │  ← banner
│   ed negative feedback. Filters disabled; │
│   click an item to adjudicate.            │
├───────────────────────────────────────────┤
│ build #0   aggregate   [ score 4.2 · 3 ac…│  ← warn badge: evidence_score · actors
│   用 cargo build --release 构建 innate…    │
│   repeated task_fail on this chunk         │  ← reason (muted)
│ ───────────────────────────────────────── │
│ recall #3  decay       [ score 2.1 · 1 ac…│
│   ...                                      │
├───────────────────────────────────────────┤
│ [ ‹ Prev ]   2 flagged   [ Next › ]        │  ← pager disabled in this mode
└───────────────────────────────────────────┘
   (clicking a proposal opens the same detail pane as View A)
```

### View B — LLM Traces: fixed list  +  scrolling call detail

```
┌──────────── list pane (fixed) ───────────┬──────── detail pane (scrolls) ────────┐
│ [ kind: all ▾ ] [ status: all ▾ ] [Reload]│  embedding                    [ ok ]  │
├───────────────────────────────────────────┤  time      2026-06-13 12:47:52        │
│ embedding  text-embedding-v4      [ ok ] ▓ │  model     text-embedding-v4          │
│   2026-06-13 12:47:52 · 142ms · 1 try ·…   │  host      dashscope.aliyuncs.com     │
│ ───────────────────────────────────────── │  latency   142 ms                     │
│ chat  qwen3.7-max               [ warn ]   │  attempts  1                          │
│   2026-06-13 12:40:11 · 1203ms · 2 try     │  tokens    { total_tokens: 1234 }     │
│ ───────────────────────────────────────── │                                        │
│        ⋮                                   │  error (only if failed)               │
│                                            │  ┌── red pre ───────────────────────┐ │
│                                            │  │ HTTP error: status 429 …         │ │
│                                            │  └──────────────────────────────────┘ │
│                                            │  request                               │
│                                            │  ┌──────────────────────────────────┐ │
│                                            │  │ { pretty-printed JSON }          │ │
│                                            │  └──────────────────────────────────┘ │
├───────────────────────────────────────────┤  response                              │
│              "12 calls"                    │  ┌──────────────────────────────────┐ │
└───────────────────────────────────────────┴──┴──────────────────────────────────┴──┘
```

### Transient — Toast (fixed, bottom-center, auto-dismiss ~3.2s)

```
                         ┌────────────────────────┐
                         │  approve ok            │   (ok=green / err=red border)
                         └────────────────────────┘
```

