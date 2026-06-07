#!/usr/bin/env bash
# Poll open GitHub issues for actuallyroy/aether-editor and alert on anything
# newer than the last-seen number. Runs standalone (no Claude /loop) — start it
# in the background and tail the log:
#
#   nohup tools/issue-poller.sh >/tmp/aether-issues.log 2>&1 &
#   tail -f /tmp/aether-issues.log
#
# Override defaults via env: INTERVAL (seconds), REPO, STATE_FILE.
set -euo pipefail

REPO="${REPO:-actuallyroy/aether-editor}"
INTERVAL="${INTERVAL:-1200}"            # 20 min
STATE_FILE="${STATE_FILE:-$HOME/.aether/last-issue.txt}"

mkdir -p "$(dirname "$STATE_FILE")"
# Seed the baseline to the known pre-existing highest issue (#35) on first run.
last_seen="$(cat "$STATE_FILE" 2>/dev/null || echo 35)"

notify() {
  local title="$1" body="$2"
  command -v osascript >/dev/null 2>&1 &&
    osascript -e "display notification \"${body//\"/\\\"}\" with title \"${title//\"/\\\"}\"" >/dev/null 2>&1 || true
}

echo "[$(date '+%F %T')] watching $REPO every ${INTERVAL}s (baseline #$last_seen)"
while true; do
  # Highest open issue number, or empty on API error (skip this round).
  newest="$(gh issue list --repo "$REPO" --state open --limit 1 \
              --json number --jq '.[0].number' 2>/dev/null || echo '')"
  if [[ -n "$newest" && "$newest" =~ ^[0-9]+$ && "$newest" -gt "$last_seen" ]]; then
    while IFS=$'\t' read -r num title author; do
      [[ "$num" -gt "$last_seen" ]] || continue
      echo "[$(date '+%F %T')] NEW #$num by $author: $title"
      notify "Aether issue #$num" "$title (@$author)"
    done < <(gh issue list --repo "$REPO" --state open --limit 30 \
               --json number,title,author --jq \
               '.[] | "\(.number)\t\(.title)\t\(.author.login)"' | sort -n)
    last_seen="$newest"
    echo "$last_seen" > "$STATE_FILE"
  fi
  sleep "$INTERVAL"
done
