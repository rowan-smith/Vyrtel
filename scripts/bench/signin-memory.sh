#!/usr/bin/env bash
# Sign-in memory (AUTH-PERF-04): start the given Vyrtel binary on a fresh data
# directory, send bursts of sign-ins (sequential, concurrent, wrong
# passwords, and a wrong-password flood while steady clients ingest and
# query), sample the server's RSS every 100 ms, and check the results
# against scripts/bench/signin-budgets.md.
#
#   scripts/bench/signin-memory.sh BINARY
#
# Linux only (reads /proc). Needs curl and awk. Writes $OUT (default
# target/signin-memory): rss.csv, one <phase>.tsv per phase (HTTP status and
# seconds per attempt), steady.tsv (the steady clients), summary.json, and
# the verdicts on stdout. Exits 1 when a budget is missed. Uses the default
# allocator: do not set MALLOC_* for a measurement. FLOOD_S sets how long
# the wrong-password flood beside the steady clients lasts (default 20 s).
set -euo pipefail

[ $# -eq 1 ] || { echo "usage: $0 BINARY" >&2; exit 2; }
BINARY=$1
OUT=${OUT:-target/signin-memory}
FLOOD_S=${FLOOD_S:-20}   # seconds of the wrong-password flood beside the steady clients
PORT=${PORT:-$((20000 + RANDOM % 20000))}
B="http://127.0.0.1:$PORT"
PASSWORD="signin-$RANDOM$RANDOM$RANDOM"
WORK=$(mktemp -d "${TMPDIR:-/tmp}/vyrtel-signin.XXXXXX")
rm -rf "$OUT"; mkdir -p "$OUT"
trap 'kill -9 $(jobs -p) 2>/dev/null || true; rm -rf "$WORK"' EXIT
env | grep -q '^MALLOC_' && { echo "unset MALLOC_* first: the budgets are for the default allocator" >&2; exit 2; }

VYRTEL_AUTH_ADMIN_PASSWORD=$PASSWORD VYRTEL_ALERTS_ENABLED=false \
  "$BINARY" --data-dir "$WORK/data" --bind "127.0.0.1:$PORT" --log-level warn >"$OUT/server.log" 2>&1 &
PID=$!
for _ in $(seq 200); do curl -fsS "$B/ready" >/dev/null 2>&1 && break; sleep 0.1; done
curl -fsS "$B/ready" >/dev/null || { echo "server did not become ready" >&2; exit 1; }

rss() { awk '/^VmRSS:/ { print $2 }' "/proc/$PID/status"; }
( while kill -0 "$PID" 2>/dev/null; do printf '%s,%s\n' "$(date +%s.%N)" "$(rss)"; sleep 0.1; done ) >"$OUT/rss.csv" &
sleep 2
IDLE=$(rss)

login() {  # login <password> -> "status seconds"
  curl -s -o /dev/null -w '%{http_code}\t%{time_total}\n' -X POST "$B/api/v1/auth/login" \
    -H 'content-type: application/json' -d "{\"username\":\"admin\",\"password\":\"$1\"}"
}
export -f login; export B
phase() {  # phase <name> <attempts> <at once> <password>
  seq "$2" | xargs -P "$3" -I{} bash -c "login '$4'" >"$OUT/$1.tsv"
  sleep 1
  printf '%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$([ "$4" = "$PASSWORD" ] && echo right || echo wrong)" >>"$OUT/phases.tsv"
}
: >"$OUT/phases.tsv"
START=$(date +%s.%N)
phase sequential 250 1 "$PASSWORD"
phase concurrent-64 256 64 "$PASSWORD"
phase wrong-64 256 64 wrong-password
phase concurrent-128 768 128 "$PASSWORD"

# A wrong-password flood while steady clients ingest and query (a session
# taken before the flood), and a genuine sign-in retries until it succeeds.
JAR="$WORK/cookies"
curl -s -o /dev/null -c "$JAR" -X POST "$B/api/v1/auth/login" -H 'content-type: application/json' \
  -d "{\"username\":\"admin\",\"password\":\"$PASSWORD\"}"
steady() {
  while [ ! -e "$WORK/stop" ]; do
    curl -s -o /dev/null -w 'ingest\t%{http_code}\t%{time_total}\n' -X POST "$B/api/v1/events" \
      -H 'content-type: application/json' -d '{"level":"Information","service":"signin-memory","message":"steady"}' || echo -e 'ingest\t000\t0'
    curl -s -o /dev/null -w 'query\t%{http_code}\t%{time_total}\n' -b "$JAR" -X POST "$B/api/v1/query/logs/count" \
      -H 'content-type: application/json' -d '{"query":"service = \"signin-memory\"","from":"-1h"}' || echo -e 'query\t000\t0'
    sleep 0.05
  done >>"$OUT/steady.tsv"
}
: >"$OUT/steady.tsv"
steady & STEADY=$!
: >"$OUT/flood-wrong-128.tsv"
( flood_end=$((SECONDS + FLOOD_S))
  while [ $SECONDS -lt $flood_end ]; do seq 256 | xargs -P 128 -I{} bash -c "login wrong-password"; done
) >>"$OUT/flood-wrong-128.tsv" &
FLOOD=$!
sleep 1
GENUINE_START=$(date +%s.%N); GENUINE_TRIES=0
until [ "$(login "$PASSWORD" | cut -f1)" = 200 ]; do GENUINE_TRIES=$((GENUINE_TRIES + 1)); sleep 0.1; done
GENUINE_S=$(awk -v a="$GENUINE_START" -v b="$(date +%s.%N)" 'BEGIN { printf "%.2f", b - a }')
wait "$FLOOD"
touch "$WORK/stop"; wait "$STEADY"
printf 'flood-wrong-128\t%s\t128\twrong\n' "$(wc -l <"$OUT/flood-wrong-128.tsv")" >>"$OUT/phases.tsv"
END=$(date +%s.%N)

sleep 10
AFTER=$(rss)
kill "$PID"; wait "$PID" 2>/dev/null || true

# ---- Report ----
PEAK=$(awk -F, -v s="$START" -v e="$END" '$1 >= s && $1 <= e && $2 > m { m = $2 } END { print m + 0 }' "$OUT/rss.csv")
mib() { awk -v k="$1" 'BEGIN { printf "%.1f", k / 1024 }'; }
PEAK_UP=$(awk -v p="$PEAK" -v i="$IDLE" 'BEGIN { printf "%.1f", (p - i) / 1024 }')
AFTER_UP=$(awk -v a="$AFTER" -v i="$IDLE" 'BEGIN { printf "%.1f", (a - i) / 1024 }')
all="$OUT/all-signins.tsv"; : >"$all"
while IFS=$'\t' read -r name _ _ pw; do awk -v pw="$pw" -F'\t' '{ print pw "\t" $1 "\t" $2 }' "$OUT/$name.tsv" >>"$all"; done <"$OUT/phases.tsv"
UNEXPECTED=$(awk -F'\t' '!(($1 == "right" && ($2 == 200 || $2 == 429)) || ($1 == "wrong" && ($2 == 401 || $2 == 429)))' "$all" | wc -l)
REFUSED=$(cat "$OUT/concurrent-64.tsv" "$OUT/wrong-64.tsv" "$OUT/concurrent-128.tsv" "$OUT/flood-wrong-128.tsv" | awk -F'\t' '$1 == 429' | wc -l)
P99_429=$(awk -F'\t' '$2 == 429 { print $3 * 1000 }' "$all" | sort -n | awk '{ v[NR] = $1 } END { if (NR == 0) print 0; else { i = int(NR * 0.99 + 0.999); if (i > NR) i = NR; printf "%.1f", v[i] } }')
STEADY_BAD=$(awk -F'\t' '$2 == "000" || $2 >= 500' "$OUT/steady.tsv" | wc -l)
STEADY_N=$(wc -l <"$OUT/steady.tsv")
counts() { awk -F'\t' '{ c[$1]++ } END { s = ""; for (k in c) s = s sprintf("%s\"%s\": %d", s ? ", " : "", k, c[k]); print "{" s "}" }' "$1"; }

verdict() { if awk "BEGIN { exit !($1) }"; then echo PASS; else echo FAIL; fi; }
V1=$(verdict "$PEAK_UP <= 30"); V2=$(verdict "$AFTER_UP <= 8"); V3=$(verdict "$UNEXPECTED == 0")
V4=$(verdict "$REFUSED > 0"); V5=$(verdict "$P99_429 <= 100"); V6=$(verdict "$STEADY_BAD == 0")
# Decided here, not inside the $(...) above: a subshell cannot set it.
FAILED=0
case " $V1 $V2 $V3 $V4 $V5 $V6 " in *" FAIL "*) FAILED=1 ;; esac

{
  echo "{"
  echo "  \"binary\": \"$("$BINARY" --version 2>/dev/null | head -1)\","
  echo "  \"idleRssMiB\": $(mib "$IDLE"), \"peakRssMiB\": $(mib "$PEAK"), \"afterRssMiB\": $(mib "$AFTER"),"
  echo "  \"peakAboveIdleMiB\": $PEAK_UP, \"afterAboveIdleMiB\": $AFTER_UP,"
  echo "  \"phases\": {"
  sep=""
  while IFS=$'\t' read -r name n p pw; do
    printf '%s    "%s": {"attempts": %s, "atOnce": %s, "password": "%s", "statuses": %s}' "$sep" "$name" "$n" "$p" "$pw" "$(counts "$OUT/$name.tsv")"
    sep=$',\n'
  done <"$OUT/phases.tsv"
  echo; echo "  },"
  echo "  \"unexpectedStatuses\": $UNEXPECTED, \"refused429\": $REFUSED, \"p99Ms429\": $P99_429,"
  echo "  \"steady\": {\"requests\": $STEADY_N, \"errorsOr5xx\": $STEADY_BAD, \"ingest\": $(awk -F'\t' '$1 == "ingest" { print $2 }' "$OUT/steady.tsv" | sort | uniq -c | awk '{ s = s sprintf("%s\"%s\": %d", s ? ", " : "", $2, $1) } END { print "{" s "}" }'), \"query\": $(awk -F'\t' '$1 == "query" { print $2 }' "$OUT/steady.tsv" | sort | uniq -c | awk '{ s = s sprintf("%s\"%s\": %d", s ? ", " : "", $2, $1) } END { print "{" s "}" }')},"
  echo "  \"genuineSignInDuringFlood\": {\"seconds\": $GENUINE_S, \"refusedFirst\": $GENUINE_TRIES},"
  echo "  \"pass\": $([ $FAILED = 0 ] && echo true || echo false)"
  echo "}"
} >"$OUT/summary.json"

cat <<EOF
Idle RSS $(mib "$IDLE") MiB; peak $(mib "$PEAK") MiB; 10 s after $(mib "$AFTER") MiB
| Budget | Limit | Measured | Verdict |
|---|---|---|---|
| Peak RSS above idle | ≤ 30 MiB | $PEAK_UP MiB | $V1 |
| RSS 10 s after the last attempt, above idle | ≤ 8 MiB | $AFTER_UP MiB | $V2 |
| Unexpected sign-in statuses | 0 | $UNEXPECTED | $V3 |
| 429s when over the admission limit | > 0 | $REFUSED | $V4 |
| 429 response time p99 | ≤ 100 ms | $P99_429 ms | $V5 |
| Steady clients' errors and 5xx during the flood | 0 | $STEADY_BAD of $STEADY_N | $V6 |
Genuine sign-in during the flood: ${GENUINE_S} s, after $GENUINE_TRIES refusals (recorded)
EOF
exit $FAILED
