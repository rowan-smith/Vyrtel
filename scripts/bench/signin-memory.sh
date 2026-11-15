#!/usr/bin/env bash
# Sign-in memory (AUTH-PERF-04): start the given Vyrtel binary on a fresh data
# directory, send bursts of sign-ins (sequential, concurrent, wrong
# passwords, and a wrong-password flood while steady clients ingest and
# query), sample the server's RSS every 100 ms, and check the results
# against scripts/bench/signin-budgets.md.
#
#   scripts/bench/signin-memory.sh BINARY
#
# Linux only (reads /proc). Needs curl and awk. Writes a new results
# directory, $OUT (default target/signin-memory/<UTC time>; an existing,
# non-empty one is refused, so earlier evidence is never overwritten):
# rss.csv, one <phase>.tsv per phase (HTTP status and seconds per attempt),
# steady.tsv (the steady clients), summary.json, and the verdicts on stdout.
#
# A run is evidence only when it is valid: the setup sign-in and an
# authenticated query and ingest succeed; right passwords got 200s and
# wrong ones 401s in every phase; the steady clients had successful ingest
# and query requests during the flood and no status other than 200 (and 429
# for ingest, counted apart); a genuine sign-in got in within
# GENUINE_DEADLINE_S; and the server was still running at the end. Exits 1
# when the run is invalid or a budget is missed. Every request has a
# connect timeout (5 s) and a total timeout (30 s), so a candidate that
# hangs fails the run instead of hanging it.
#
# Uses the default allocator: do not set MALLOC_* for a measurement.
# FLOOD_S sets how long the wrong-password flood beside the steady clients
# lasts (default 20 s); GENUINE_DEADLINE_S how long the genuine sign-in may
# take (default 30 s).
set -euo pipefail

[ $# -eq 1 ] || { echo "usage: $0 BINARY" >&2; exit 2; }
BINARY=$1
OUT=${OUT:-target/signin-memory/$(date -u +%Y%m%dT%H%M%SZ)}
FLOOD_S=${FLOOD_S:-20}
GENUINE_DEADLINE_S=${GENUINE_DEADLINE_S:-30}
PORT=${PORT:-$((20000 + RANDOM % 20000))}
B="http://127.0.0.1:$PORT"
PASSWORD="signin-$RANDOM$RANDOM$RANDOM"
CURL=(curl -s --connect-timeout 5 --max-time 30)
env | grep -q '^MALLOC_' && { echo "unset MALLOC_* first: the budgets are for the default allocator" >&2; exit 2; }
if [ -d "$OUT" ] && [ -n "$(ls -A "$OUT")" ]; then
  echo "$OUT already holds results; choose another OUT (nothing is overwritten)" >&2; exit 2
fi
mkdir -p "$OUT"
WORK=$(mktemp -d "${TMPDIR:-/tmp}/vyrtel-signin.XXXXXX")
trap 'kill -9 $(jobs -p) 2>/dev/null || true; rm -rf "$WORK"' EXIT
: >"$OUT/invalid.txt"
invalid() { echo "$1" >>"$OUT/invalid.txt"; }

VYRTEL_AUTH_ADMIN_PASSWORD=$PASSWORD VYRTEL_ALERTS_ENABLED=false \
  "$BINARY" --data-dir "$WORK/data" --bind "127.0.0.1:$PORT" --log-level warn >"$OUT/server.log" 2>&1 &
PID=$!
for _ in $(seq 200); do "${CURL[@]}" -f "$B/ready" >/dev/null 2>&1 && break; sleep 0.1; done
"${CURL[@]}" -f "$B/ready" >/dev/null || { echo "INVALID: the server did not become ready within 20 s" >&2; exit 1; }

rss() { awk '/^VmRSS:/ { print $2 }' "/proc/$PID/status"; }
( while kill -0 "$PID" 2>/dev/null; do printf '%s,%s\n' "$(date +%s.%N)" "$(rss)"; sleep 0.1; done ) >"$OUT/rss.csv" &

# Setup: the session the steady query client uses, checked before anything is measured.
JAR="$WORK/cookies"
setup=$("${CURL[@]}" -o /dev/null -w '%{http_code}' -c "$JAR" -X POST "$B/api/v1/auth/login" \
  -H 'content-type: application/json' -d "{\"username\":\"admin\",\"password\":\"$PASSWORD\"}" || true)
ingest_once() {
  "${CURL[@]}" -o /dev/null -w 'ingest\t%{http_code}\t%{time_total}\n' -X POST "$B/api/v1/events" \
    -H 'content-type: application/json' -d '{"level":"Information","service":"signin-memory","message":"steady"}' || true
}
query_once() {
  "${CURL[@]}" -o /dev/null -w 'query\t%{http_code}\t%{time_total}\n' -b "$JAR" -X POST "$B/api/v1/query/logs/count" \
    -H 'content-type: application/json' -d '{"query":"service = \"signin-memory\"","from":"-1h"}' || true
}
pre_ingest=$(ingest_once | cut -f2); pre_query=$(query_once | cut -f2)
[ "$setup" = 200 ] || invalid "setup sign-in returned $setup, not 200"
[ "$pre_query" = 200 ] || invalid "authenticated query before the run returned $pre_query, not 200"
[ "$pre_ingest" = 200 ] || invalid "ingest before the run returned $pre_ingest, not 200"
if [ -s "$OUT/invalid.txt" ]; then
  echo "INVALID: $(paste -sd ';' "$OUT/invalid.txt")" >&2; exit 1
fi
sleep 2
IDLE=$(rss)

login() {  # login <password> -> "status seconds" (000 when it failed or timed out)
  curl -s --connect-timeout 5 --max-time 30 -o /dev/null -w '%{http_code}\t%{time_total}\n' -X POST "$B/api/v1/auth/login" \
    -H 'content-type: application/json' -d "{\"username\":\"admin\",\"password\":\"$1\"}" || true
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

# A wrong-password flood while steady clients ingest and query, and a
# genuine sign-in retries until it gets in or its deadline passes.
steady() {
  while [ ! -e "$WORK/stop" ]; do ingest_once; query_once; sleep 0.05; done >>"$OUT/steady.tsv"
}
: >"$OUT/steady.tsv"
steady & STEADY=$!
: >"$OUT/flood-wrong-128.tsv"
( flood_end=$((SECONDS + FLOOD_S))
  while [ $SECONDS -lt $flood_end ]; do seq 256 | xargs -P 128 -I{} bash -c "login wrong-password"; done
) >>"$OUT/flood-wrong-128.tsv" &
FLOOD=$!
sleep 1
GENUINE_START=$(date +%s.%N); GENUINE_TRIES=0; GENUINE_OK=false
genuine_end=$((SECONDS + GENUINE_DEADLINE_S))
while [ $SECONDS -lt $genuine_end ]; do
  if [ "$(login "$PASSWORD" | cut -f1)" = 200 ]; then GENUINE_OK=true; break; fi
  GENUINE_TRIES=$((GENUINE_TRIES + 1)); sleep 0.1
done
GENUINE_S=$(awk -v a="$GENUINE_START" -v b="$(date +%s.%N)" 'BEGIN { printf "%.2f", b - a }')
$GENUINE_OK || invalid "a genuine sign-in did not get in within ${GENUINE_DEADLINE_S} s ($GENUINE_TRIES attempts)"
wait "$FLOOD"
touch "$WORK/stop"; wait "$STEADY"
printf 'flood-wrong-128\t%s\t128\twrong\n' "$(wc -l <"$OUT/flood-wrong-128.tsv")" >>"$OUT/phases.tsv"
END=$(date +%s.%N)

sleep 10
if kill -0 "$PID" 2>/dev/null; then
  AFTER=$(rss); kill "$PID"; wait "$PID" 2>/dev/null || true
else
  AFTER=0; invalid "the server exited during the run (see server.log)"
fi

# ---- Report ----
PEAK=$(awk -F, -v s="$START" -v e="$END" '$1 >= s && $1 <= e && $2 > m { m = $2 } END { print m + 0 }' "$OUT/rss.csv")
mib() { awk -v k="$1" 'BEGIN { printf "%.1f", k / 1024 }'; }
PEAK_UP=$(awk -v p="$PEAK" -v i="$IDLE" 'BEGIN { printf "%.1f", (p - i) / 1024 }')
AFTER_UP=$(awk -v a="$AFTER" -v i="$IDLE" 'BEGIN { printf "%.1f", (a - i) / 1024 }')
all="$OUT/all-signins.tsv"; : >"$all"
while IFS=$'\t' read -r name _ _ pw; do
  awk -v pw="$pw" -F'\t' '{ print pw "\t" $1 "\t" $2 }' "$OUT/$name.tsv" >>"$all"
  # Each phase must show the outcome its password should get, not only refusals.
  want=$([ "$pw" = right ] && echo 200 || echo 401)
  got=$(awk -F'\t' -v w="$want" '$1 == w' "$OUT/$name.tsv" | wc -l)
  [ "$got" -gt 0 ] || invalid "phase $name ($pw password) got no $want"
done <"$OUT/phases.tsv"
UNEXPECTED=$(awk -F'\t' '!(($1 == "right" && ($2 == 200 || $2 == 429)) || ($1 == "wrong" && ($2 == 401 || $2 == 429)))' "$all" | wc -l)
REFUSED=$(cat "$OUT/concurrent-64.tsv" "$OUT/wrong-64.tsv" "$OUT/concurrent-128.tsv" "$OUT/flood-wrong-128.tsv" | awk -F'\t' '$1 == 429' | wc -l)
P99_429=$(awk -F'\t' '$2 == 429 { print $3 * 1000 }' "$all" | sort -n | awk '{ v[NR] = $1 } END { if (NR == 0) print 0; else { i = int(NR * 0.99 + 0.999); if (i > NR) i = NR; printf "%.1f", v[i] } }')
STEADY_N=$(wc -l <"$OUT/steady.tsv")
STEADY_BAD=$(awk -F'\t' '$2 == "000" || $2 >= 500' "$OUT/steady.tsv" | wc -l)
INGEST_OK=$(awk -F'\t' '$1 == "ingest" && $2 == 200' "$OUT/steady.tsv" | wc -l)
INGEST_429=$(awk -F'\t' '$1 == "ingest" && $2 == 429' "$OUT/steady.tsv" | wc -l)
QUERY_OK=$(awk -F'\t' '$1 == "query" && $2 == 200' "$OUT/steady.tsv" | wc -l)
STEADY_OTHER=$(awk -F'\t' '!(($1 == "ingest" && ($2 == 200 || $2 == 429)) || ($1 == "query" && $2 == 200))' "$OUT/steady.tsv" | wc -l)
[ "$INGEST_OK" -gt 0 ] || invalid "no steady ingest request succeeded during the flood"
[ "$QUERY_OK" -gt 0 ] || invalid "no steady query succeeded during the flood"
[ "$STEADY_OTHER" -eq 0 ] || invalid "$STEADY_OTHER steady requests got a status other than 200 (or 429 for ingest)"
counts() { awk -F'\t' -v col="${2:-1}" '{ c[$col]++ } END { s = ""; for (k in c) s = s sprintf("%s\"%s\": %d", s ? ", " : "", k, c[k]); print "{" s "}" }' "$1"; }

verdict() { if awk "BEGIN { exit !($1) }"; then echo PASS; else echo FAIL; fi; }
V1=$(verdict "$PEAK_UP <= 30"); V2=$(verdict "$AFTER_UP <= 8"); V3=$(verdict "$UNEXPECTED == 0")
V4=$(verdict "$REFUSED > 0"); V5=$(verdict "$P99_429 <= 100"); V6=$(verdict "$STEADY_BAD == 0")
# Decided here, not inside the $(...) above: a subshell cannot set it.
VALID=true; [ -s "$OUT/invalid.txt" ] && VALID=false
FAILED=0
case " $V1 $V2 $V3 $V4 $V5 $V6 " in *" FAIL "*) FAILED=1 ;; esac
$VALID || FAILED=1

json_list() { awk '{ gsub(/"/, "\\\""); s = s sprintf("%s\"%s\"", NR > 1 ? ", " : "", $0) } END { print "[" s "]" }' "$1"; }
{
  echo "{"
  echo "  \"binary\": \"$("$BINARY" --version 2>/dev/null | head -1)\","
  echo "  \"valid\": $VALID, \"invalidReasons\": $(json_list "$OUT/invalid.txt"),"
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
  echo "  \"steady\": {\"requests\": $STEADY_N, \"errorsOr5xx\": $STEADY_BAD, \"ingestOk\": $INGEST_OK, \"ingest429\": $INGEST_429, \"queryOk\": $QUERY_OK, \"otherStatuses\": $STEADY_OTHER},"
  echo "  \"genuineSignInDuringFlood\": {\"succeeded\": $GENUINE_OK, \"seconds\": $GENUINE_S, \"refusedFirst\": $GENUINE_TRIES},"
  echo "  \"pass\": $([ $FAILED = 0 ] && echo true || echo false)"
  echo "}"
} >"$OUT/summary.json"

cat <<EOF
Results in $OUT
Idle RSS $(mib "$IDLE") MiB; peak $(mib "$PEAK") MiB; 10 s after $(mib "$AFTER") MiB
Run valid: $VALID$($VALID || echo " ($(paste -sd ';' "$OUT/invalid.txt"))")
Steady clients during the flood: $INGEST_OK ingest 200s ($INGEST_429 ingest 429s), $QUERY_OK query 200s, $STEADY_OTHER other statuses
| Budget | Limit | Measured | Verdict |
|---|---|---|---|
| Peak RSS above idle | ≤ 30 MiB | $PEAK_UP MiB | $V1 |
| RSS 10 s after the last attempt, above idle | ≤ 8 MiB | $AFTER_UP MiB | $V2 |
| Unexpected sign-in statuses | 0 | $UNEXPECTED | $V3 |
| 429s when over the admission limit | > 0 | $REFUSED | $V4 |
| 429 response time p99 | ≤ 100 ms | $P99_429 ms | $V5 |
| Steady clients' errors and 5xx during the flood | 0 | $STEADY_BAD of $STEADY_N | $V6 |
Genuine sign-in during the flood: $($GENUINE_OK && echo "${GENUINE_S} s, after $GENUINE_TRIES refusals" || echo "none within ${GENUINE_DEADLINE_S} s") (recorded)
EOF
exit $FAILED
