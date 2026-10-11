#!/usr/bin/env bash
# Restore drill (REL-06): follow the backup and restore procedure in
# docs/configuration.md#backup-and-restore against a real Vyrtel binary and
# check that every documented verification query answers as it did before
# the backup.
#
#   scripts/backup/restore-drill.sh [path/to/vyrtel]     (default: target/release/vyrtel)
#
# Linux and macOS (needs bash, curl, tar, GNU or BSD date). Run from the
# repository root; it uses tests/fixtures. Exits non-zero on the first
# failed check and keeps its working directory for inspection.
set -euo pipefail

VYRTEL=${1:-target/release/vyrtel}
FIXTURES=tests/fixtures
PASSWORD=drill-password
TRACE=4bf92f3577b34da6a3ce929d0e0e4736
WORK=$(mktemp -d "${TMPDIR:-/tmp}/vyrtel-restore-drill.XXXXXX")
PORT=$((20000 + RANDOM % 20000))
URL="http://127.0.0.1:$PORT"
PID=

step() { printf '\n== %s\n' "$*"; }
fail() { printf 'FAIL: %s\n(working directory kept: %s)\n' "$*" "$WORK" >&2; exit 1; }
cleanup() { if [ -n "$PID" ]; then kill -9 "$PID" 2>/dev/null || true; fi; }
trap cleanup EXIT

[ -x "$VYRTEL" ] || fail "no executable at $VYRTEL (build it with: cargo build --release -p server)"

# Start Vyrtel on a data directory and wait until it answers /health.
start() {
  local dir=$1; shift
  env VYRTEL_AUTH_ENABLED=true "$@" "$VYRTEL" --data-dir "$dir" --bind "127.0.0.1:$PORT" --log-level warn \
    >"$WORK/stdout.log" 2>>"$WORK/stderr.log" &
  PID=$!
  for _ in $(seq 1 100); do
    curl -fs "$URL/health" >/dev/null 2>&1 && return 0
    kill -0 "$PID" 2>/dev/null || fail "vyrtel exited during startup: $(cat "$WORK/stderr.log")"
    sleep 0.1
  done
  fail "vyrtel did not become healthy"
}

# Step 1 of the procedure: stop with SIGTERM and require exit status 0.
stop() {
  kill -TERM "$PID"
  local status=0
  wait "$PID" || status=$?
  PID=
  [ "$status" -eq 0 ] || fail "vyrtel exited with status $status: $(tail -5 "$WORK/stderr.log")"
}

# Replace {{now-<n><unit>}} placeholders with Unix nanoseconds (tests/fixtures/README.md).
fixture() {
  local text now ph n ns
  text=$(cat "$FIXTURES/$1")
  now=$(( $(date +%s) * 1000000000 ))
  for ph in $(grep -o '{{now-[0-9]*m\{0,1\}s\{0,1\}}}' "$FIXTURES/$1" | sort -u); do
    n=${ph#\{\{now-}; n=${n%\}\}}
    case $n in
      *ms) ns=$(( ${n%ms} * 1000000 )) ;;
      *s) ns=$(( ${n%s} * 1000000000 )) ;;
      *m) ns=$(( ${n%m} * 60000000000 )) ;;
    esac
    text=${text//"$ph"/$(( now - ns ))}
  done
  printf '%s' "$text"
}

# curl as the logged-in admin; prints the body, fails on a non-2xx status.
admin() { curl -fsS -b "$WORK/cookies" -H 'content-type: application/json' "$@"; }
status() { curl -s -o /dev/null -w '%{http_code}' "$@"; }

login() {
  [ "$(status -c "$WORK/cookies" -H 'content-type: application/json' \
      -d "{\"username\":\"admin\",\"password\":\"$PASSWORD\"}" "$URL/api/v1/auth/login")" = 200 ] \
    || fail "admin login refused"
}

# The documented verification queries. Writes one line per answer to $1,
# keeping only the fields that do not change between runs (no timings).
verify() {
  local out=$1 signal
  : >"$out"
  for signal in logs traces metrics; do
    echo "$signal $(admin -d '{"query":""}' "$URL/api/v1/query/$signal/count" | grep -o '"count":[0-9]*')" >>"$out"
  done
  admin -d '{"query":"level = Error","limit":100}' "$URL/api/v1/query/logs" \
    | grep -o '"message":"[^"]*"' | sort >>"$out"
  admin "$URL/api/v1/traces/$TRACE" | grep -o '"spanId":"[^"]*"' | sort >>"$out"
  admin "$URL/api/v1/metrics" >>"$out"; echo >>"$out"
  admin "$URL/api/v1/dashboards" >>"$out"; echo >>"$out"
  admin "$URL/api/v1/saved-queries" >>"$out"; echo >>"$out"
  admin "$URL/api/v1/alerts" | grep -Eo '"(name|query|op|threshold|enabled)":[^,}]*' >>"$out"
  admin "$URL/api/v1/api-keys" | grep -Eo '"(name|prefix|scope)":"[^"]*"' >>"$out"
}

step "Populate an instance ($VYRTEL, $("$VYRTEL" --version))"
start "$WORK/data" VYRTEL_AUTH_ADMIN_PASSWORD="$PASSWORD"
login
KEY=$(admin -d '{"name":"producers"}' "$URL/api/v1/api-keys" | grep -o '"key":"vyr_[0-9a-f]*"' | cut -d'"' -f4)
[ -n "$KEY" ] || fail "no API key created"
ingest() { curl -fsS -o /dev/null -H "authorization: Bearer $KEY" -H "content-type: $2" --data-binary "$3" "$URL$1"; }
ingest /api/v1/events application/x-ndjson "$(cat "$FIXTURES/logs.ndjson")"
ingest /v1/logs application/json "$(fixture otlp-logs.json)"
ingest /v1/traces application/json "$(fixture otlp-traces.json)"
ingest /v1/metrics application/json "$(fixture otlp-metrics.json)"
DASH=$(admin -d '{"name":"Checkout","description":"restore drill"}' "$URL/api/v1/dashboards" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2)
admin -o /dev/null -d '{"title":"Errors","kind":"log_count","config":{"query":"level = Error"}}' "$URL/api/v1/dashboards/$DASH/panels"
admin -o /dev/null -d '{"name":"Error spike","query":"level = Error","op":">","threshold":5,"windowSecs":300,"intervalSecs":60,"enabled":false}' "$URL/api/v1/alerts"
admin -o /dev/null -d '{"name":"Inventory warnings","signal":"logs","query":"level = Warning"}' "$URL/api/v1/saved-queries"
verify "$WORK/before.txt"
grep -q '^logs "count":[1-9]' "$WORK/before.txt" || fail "logs were not stored: $(cat "$WORK/before.txt")"
grep -q '^traces "count":[1-9]' "$WORK/before.txt" || fail "traces were not stored"
grep -q '^metrics "count":[1-9]' "$WORK/before.txt" || fail "metrics were not stored"

step "1. Stop Vyrtel (SIGTERM, exit status 0)"
stop

step "2. Back up the whole data directory"
tar -C "$WORK/data" -czf "$WORK/backup.tar.gz" .
tar -tzf "$WORK/backup.tar.gz" | grep -q '^\./vyrtel.json$' || fail "the backup has no manifest"
rm -rf "$WORK/data"   # the original host is gone

step "3. Restore on a clean host and start (no admin password configured)"
mkdir "$WORK/restored"
tar -C "$WORK/restored" -xzf "$WORK/backup.tar.gz"
start "$WORK/restored"
grep -q 'Admin login' "$WORK/stdout.log" && fail "a new admin password was generated: the restored user was not used"

step "4. Verification queries"
login
verify "$WORK/after.txt"
diff -u "$WORK/before.txt" "$WORK/after.txt" || fail "verification answers differ after restore"
[ "$(admin "$URL/api/v1/system/storage" | grep -o '"quarantined":\[\]' | wc -l | tr -d ' ')" = 3 ] \
  || fail "recovery quarantined files from a stopped backup"
[ "$(status -H "authorization: Bearer $KEY" -H 'content-type: application/json' -d '{"message":"after restore"}' "$URL/api/v1/events")" = 200 ] \
  || fail "the restored API key was refused"
[ "$(status -H 'content-type: application/json' -d '{}' "$URL/api/v1/events")" = 401 ] \
  || fail "ingest without a key was accepted"
[ "$(status "$URL/api/v1/dashboards")" = 401 ] || fail "configuration readable without a login"
echo "verification queries match; key, login and anonymous refusal behave as before"
stop

step "5. An incomplete backup is refused without changes"
mkdir "$WORK/partial"
tar -C "$WORK/partial" -xzf "$WORK/backup.tar.gz"
rm "$WORK/partial/metadata/metadata.db"
before=$(cd "$WORK/partial" && find . -print | sort)
code=0
VYRTEL_AUTH_ENABLED=true "$VYRTEL" --data-dir "$WORK/partial" --bind "127.0.0.1:$PORT" >/dev/null 2>"$WORK/refused.log" || code=$?
[ "$code" = 1 ] || fail "expected exit status 1, got $code"
grep -q 'is incomplete: metadata/metadata.db is missing' "$WORK/refused.log" || fail "unexpected message: $(cat "$WORK/refused.log")"
[ "$(cd "$WORK/partial" && find . -print | sort)" = "$before" ] || fail "the refused directory was changed"
cat "$WORK/refused.log"

rm -rf "$WORK"
printf '\nrestore drill passed\n'
