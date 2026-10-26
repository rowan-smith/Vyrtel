#!/usr/bin/env bash
# Authenticated ingest with and without API key churn (AUTH-PERF-01): start
# each given Vyrtel binary on a fresh data directory and run `keychurn`
# against it for every cell of the matrix
#
#   producers {low, high} × request size {small, batched} × churn {off, on}
#
# RUNS times (default 3), alternating the binaries within each repetition so
# drift affects them alike. Each run records keychurn's summary.json plus
# the server's CPU seconds and peak memory in server.json; then
# scripts/bench/keychurn-report.mjs compares them against the predeclared
# budgets.
#
#   scripts/bench/keychurn.sh BASELINE CANDIDATE
#   node scripts/bench/keychurn-report.mjs "$OUT"
#
# Linux (reads /proc) or Git Bash on Windows (asks PowerShell). Needs curl,
# and target/release/keychurn (cargo build --release -p loadgen).
# Environment: RUNS, DURATION (keychurn --duration), LOW/HIGH (producers),
# SMALL/BATCHED (events per request), CHURN (pause between create+revoke
# pairs), CELLS (a regex of cells to run, e.g. '^high-batched'), OUT (results
# directory), KEYCHURN (the keychurn binary).
set -euo pipefail

[ $# -eq 2 ] || { echo "usage: $0 BASELINE CANDIDATE" >&2; exit 2; }
RUNS=${RUNS:-3}
DURATION=${DURATION:-20s}
LOW=${LOW:-4}
HIGH=${HIGH:-64}
SMALL=${SMALL:-1}
BATCHED=${BATCHED:-100}
CHURN=${CHURN:-20ms}
CELLS=${CELLS:-.}
OUT=${OUT:-target/keychurn}
KEYCHURN=${KEYCHURN:-target/release/keychurn}
PASSWORD="keychurn-$RANDOM$RANDOM"
WORK=$(mktemp -d "${TMPDIR:-/tmp}/vyrtel-keychurn.XXXXXX")
trap 'kill -9 $(jobs -p) 2>/dev/null || true' EXIT
mkdir -p "$OUT"

# CPU seconds and peak memory (bytes) of a process, as "cpu peak".
usage() {
  local pid=$1
  if [ -r "/proc/$pid/winpid" ]; then
    powershell.exe -NoProfile -Command \
      "\$p = Get-Process -Id $(cat "/proc/$pid/winpid"); '{0} {1}' -f \$p.TotalProcessorTime.TotalSeconds, \$p.PeakWorkingSet64" |
      tr -d '\r'
  else
    local tck; tck=$(getconf CLK_TCK)
    awk -v t="$tck" '{ printf "%.2f ", ($14 + $15) / t }' "/proc/$pid/stat"
    awk '/^VmHWM:/ { print $2 * 1024 }' "/proc/$pid/status"
  fi
}

run() {
  local name=$1 binary=$2 cell=$3 producers=$4 batch=$5 churn=$6 rep=$7
  local dir="$OUT/$name/$cell/run$rep" data="$WORK/data-$name-$cell-$rep"
  local port=$((20000 + RANDOM % 20000))
  mkdir -p "$dir"
  VYRTEL_AUTH_ADMIN_PASSWORD=$PASSWORD VYRTEL_ALERTS_ENABLED=false \
    "$binary" --data-dir "$data" --bind "127.0.0.1:$port" --log-level error >"$dir/server.log" 2>&1 &
  local pid=$!
  until curl -fs "http://127.0.0.1:$port/health" >/dev/null 2>&1; do sleep 0.05; done
  read -r cpu0 _ < <(usage "$pid")
  VYRTEL_AUTH_ADMIN_PASSWORD=$PASSWORD "$KEYCHURN" --target "http://127.0.0.1:$port" --duration "$DURATION" \
    --concurrency "$producers" --batch "$batch" --churn-interval "$churn" \
    --label "$name-$cell-$rep" --out "$dir" >/dev/null
  read -r cpu1 peak < <(usage "$pid")
  kill "$pid"; wait "$pid" 2>/dev/null || true
  rm -rf "$data"
  printf '{"binary":"%s","cpuSeconds":%s,"peakMemoryBytes":%s}\n' \
    "$binary" "$(awk -v a="$cpu0" -v b="$cpu1" 'BEGIN { printf "%.2f", b - a }')" "$peak" >"$dir/server.json"
  echo "keychurn: $name $cell run $rep done" >&2
}

for rep in $(seq 1 "$RUNS"); do
  for producers in "low:$LOW" "high:$HIGH"; do
    for size in "small:$SMALL" "batched:$BATCHED"; do
      for churn in "steady:0ms" "churn:$CHURN"; do
        cell="${producers%%:*}-${size%%:*}-${churn%%:*}"
        [[ $cell =~ $CELLS ]] || continue
        run baseline "$1" "$cell" "${producers#*:}" "${size#*:}" "${churn#*:}" "$rep"
        run candidate "$2" "$cell" "${producers#*:}" "${size#*:}" "${churn#*:}" "$rep"
      done
    done
  done
done
echo "results in $OUT; summarise with: node scripts/bench/keychurn-report.mjs $OUT" >&2
