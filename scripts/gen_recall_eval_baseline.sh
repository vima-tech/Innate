#!/usr/bin/env bash
# Build a recall-eval label set from the *local* library's own active chunks.
#
# What this measures: retrievability. For each active chunk it emits two probes —
# the chunk's canonical trigger phrase, and a slice of its body that is not the
# trigger — labelled with that chunk's id. A healthy ranker finds the chunk from
# both.
#
# This is a REGRESSION GUARD, not a relevance benchmark. It cannot tell you
# whether recall surfaces the *right* knowledge for a real question; it tells you
# whether a weight change (a new penalty, a re-tuned channel) broke ranking. Read
# a P@1 well below ~0.98 as "I broke something", not as "retrieval is bad".
#
# The output embeds excerpts of your own knowledge base, so it is written to
# ~/.innate/ and gitignored — it is personal data, and its chunk ids are
# meaningless in any other library. Regenerate it rather than sharing it.
#
# Usage:
#   scripts/gen_recall_eval_baseline.sh [output.jsonl]
#   innate recall-eval "$OUT" --k 10 --save
set -euo pipefail

OUT="${1:-$HOME/.innate/recall_eval_active_baseline.jsonl}"
DB="${INNATE_DB:-$HOME/.innate/data/personal.db}"

[ -f "$DB" ] || { echo "no database at $DB" >&2; exit 1; }
command -v sqlite3 >/dev/null || { echo "sqlite3 is required" >&2; exit 1; }

sqlite3 -json "$DB" "
  SELECT id, trigger_desc, content FROM chunks
   WHERE state='active' AND origin!='spark'
     AND trigger_desc IS NOT NULL
     AND length(trim(trigger_desc)) >= 8
     AND length(content) >= 200;" |
python3 -c '
import json, sys
rows = json.load(sys.stdin)
out = []
for r in rows:
    trigger = " ".join(r["trigger_desc"].split())[:90]
    if trigger:
        out.append({"query": trigger, "relevant_ids": [r["id"]], "probe": "trigger"})
    body = " ".join(r["content"].split())
    excerpt = body[60:200].strip()
    if len(excerpt) >= 40:
        out.append({"query": excerpt, "relevant_ids": [r["id"]], "probe": "body"})
for o in out:
    print(json.dumps(o, ensure_ascii=False))
sys.stderr.write(f"{len(out)} labelled queries from {len(rows)} active chunks\n")
' > "$OUT"

echo "wrote $OUT"
echo "next: innate recall-eval \"$OUT\" --k 10 --save"
