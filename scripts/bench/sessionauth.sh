#!/usr/bin/env bash
# Session-cookie against admin-key authentication (AUTH-PERF-02): start the
# given Vyrtel binary on a fresh data directory and run `sessionauth` against
# it for every cell of the matrix
#
#   workload {auth, query, mixed} × clients {low, high}
#
# once with the session cookie only and once with the admin key only, RUNS
# times (default 3). The two credentials alternate within each repetition,
# and which goes first alternates between repetitions, so drift affects them
# alike. Each run records sessionauth's summary.json plus the server's CPU
# seconds and peak memory in server.json; then
# scripts/bench/sessionauth-report.mjs checks them against the predeclared
# budgets.
#
#   scripts/bench/sessionauth.sh BINARY
#   node scripts/bench/sessionauth-report.mjs "$OUT"
#
# Linux (reads /proc) or Git Bash on Windows (asks PowerShell). Needs curl,
# and target/release/sessionauth (cargo build --release -p loadgen).
# Environment: RUNS, DURATION (sessionauth --duration), LOW/HIGH (clients),
# CELLS (a regex of cells to run, e.g. '^mixed'), OUT (results directory),
# SESSIONAUTH (the sessionauth binary), DURABILITY (storage.durability).
set -euo pipefail

[ $# -eq 1 ] || { echo "usage: $0 BINARY" >&2; exit 2; }
RUNS=${RUNS:-3}
DURATION=${DURATION:-20s}
LOW=${LOW:-4}
HIGH=${HIGH:-32}
CELLS=${CELLS:-.}
OUT=${OUT:-target/sessionauth}
SESSIONAUTH=${SESSIONAUTH:-target/release/sessionauth}
DURABILITY=${DURABILITY:-normal}
PASSWORD="sessionauth-$RANDOM$RANDOM"
WORK=$(mktemp -d "${TMPDIR:-/tmp}/vyrtel-sessionauth.XXXXXX")
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
  local cell=$1 workload=$2 clients=$3 auth=$4 rep=$5
  local dir="$OUT/$cell/$auth/run$rep" data="$WORK/data-$cell-$auth-$rep"
  local port=$((20000 + RANDOM % 20000))
  mkdir -p "$dir"
  VYRTEL_AUTH_ADMIN_PASSWORD=$PASSWORD VYRTEL_ALERTS_ENABLED=false VYRTEL_STORAGE_DURABILITY=$DURABILITY \
    "$BINARY" --data-dir "$data" --bind "127.0.0.1:$port" --log-level error >"$dir/server.log" 2>&1 &
  local pid=$!
  until curl -fs "http://127.0.0.1:$port/health" >/dev/null 2>&1; do sleep 0.05; done
  read -r cpu0 _ < <(usage "$pid")
  VYRTEL_AUTH_ADMIN_PASSWORD=$PASSWORD "$SESSIONAUTH" --target "http://127.0.0.1:$port" --duration "$DURATION" \
    --auth "$auth" --workload "$workload" --concurrency "$clients" \
    --label "$cell-$auth-$rep" --out "$dir" >/dev/null
  read -r cpu1 peak < <(usage "$pid")
  kill "$pid"; wait "$pid" 2>/dev/null || true
  rm -rf "$data"
  printf '{"binary":"%s","cpuSeconds":%s,"peakMemoryBytes":%s}\n' \
    "$BINARY" "$(awk -v a="$cpu0" -v b="$cpu1" 'BEGIN { printf "%.2f", b - a }')" "$peak" >"$dir/server.json"
  echo "sessionauth: $cell $auth run $rep done" >&2
}

BINARY=$1
for rep in $(seq 1 "$RUNS"); do
  if [ $((rep % 2)) -eq 1 ]; then order="session key"; else order="key session"; fi
  for workload in auth query mixed; do
    for clients in "low:$LOW" "high:$HIGH"; do
      cell="$workload-${clients%%:*}"
      [[ $cell =~ $CELLS ]] || continue
      for auth in $order; do
        run "$cell" "$workload" "${clients#*:}" "$auth" "$rep"
      done
    done
  done
done
echo "results in $OUT; summarise with: node scripts/bench/sessionauth-report.mjs $OUT" >&2
