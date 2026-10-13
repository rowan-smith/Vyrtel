#!/usr/bin/env bash
# Startup cost of the data-directory format check (REL-07): start each given
# Vyrtel binary on the same stopped data directories, whose telemetry is all
# in the WAL, and report the time until /health answers, the bytes read
# (/proc/<pid>/io rchar) and peak memory (VmHWM) at that point, plus the
# time and peak memory of `check-data-dir` where the binary has it.
#
#   scripts/bench/startup-preflight.sh BASELINE [CANDIDATE...]
#
# Linux only (reads /proc). Needs bash, curl, awk and GNU time
# (/usr/bin/time). The first binary writes the datasets. The page cache is
# warm (each run reads the copied WAL once first), so the times measure CPU
# and syscalls rather than the disk. Results are in docs/testing.md.
set -euo pipefail

[ $# -ge 1 ] || { echo "usage: $0 BASELINE [CANDIDATE...]" >&2; exit 2; }
RUNS=${RUNS:-5}
WORK=$(mktemp -d "${TMPDIR:-/tmp}/vyrtel-preflight.XXXXXX")
PORT=$((20000 + RANDOM % 20000))
URL="http://127.0.0.1:$PORT"
trap 'kill -9 $(jobs -p) 2>/dev/null || true' EXIT

wait_healthy() { until curl -fs "$URL/health" >/dev/null 2>&1; do sleep 0.005; done; }

# Write a dataset: <requests> NDJSON requests of <events> log events each,
# all left in the WAL by a clean stop (no seal: large target, long age).
populate() {
  local name=$1 events=$2 requests=$3 dir=$WORK/$1
  mkdir -p "$dir"
  VYRTEL_STORAGE_SEGMENT_TARGET_SIZE=2GB VYRTEL_STORAGE_SEGMENT_MAX_AGE=1h VYRTEL_STORAGE_COMPACTION=false \
    VYRTEL_RETENTION_LOGS=forever "$BASE" --data-dir "$dir" --bind "127.0.0.1:$PORT" --memory-limit 4GB \
    --log-level error >/dev/null &
  local pid=$!
  wait_healthy
  awk -v n="$events" 'BEGIN { for (i = 0; i < n; i++) printf "{\"message\":\"request %d handled in %d ms by worker %d\",\"level\":\"Info\",\"service\":\"bench\",\"attributes\":{\"route\":\"/api/items/%d\",\"status\":200,\"pad\":\"%080d\"}}\n", i, i % 997, i % 13, i % 5000, i }' \
    >"$WORK/payload.ndjson"
  # Some requests may be refused with 429 under 8 parallel uploads; the
  # dataset is whatever was stored.
  seq 1 "$requests" | xargs -P 8 -I{} curl -s -o /dev/null -H 'content-type: application/x-ndjson' \
    --data-binary @"$WORK/payload.ndjson" "$URL/api/v1/events" || true
  kill -TERM "$pid"; wait "$pid"
  echo "dataset $name: $(du -sh "$dir/wal" | cut -f1) of WAL, $events events per request, $requests requests"
}

# One start: milliseconds to healthy, MiB read, MiB peak memory.
start_once() {
  local bin=$1 dataset=$2 run=$WORK/run
  rm -rf "$run"; cp -R "$WORK/$dataset" "$run"; cat "$run"/wal/*/* >/dev/null
  local t0 t1 pid rchar hwm
  t0=$(date +%s%N)
  VYRTEL_RETENTION_LOGS=forever "$bin" --data-dir "$run" --bind "127.0.0.1:$PORT" --memory-limit 4GB \
    --log-level error >/dev/null &
  pid=$!
  wait_healthy
  t1=$(date +%s%N)
  rchar=$(awk '/^rchar/ { print $2 }' "/proc/$pid/io")
  hwm=$(awk '/VmHWM/ { print $2 }' "/proc/$pid/status")
  kill -TERM "$pid"; wait "$pid" || true
  echo "$(( (t1 - t0) / 1000000 )) $(( rchar / 1048576 )) $(( hwm / 1024 ))"
}

median() { sort -n | awk '{ a[NR] = $1 } END { print a[int((NR + 1) / 2)] }'; }

BASE=$1
populate large 4000 120
populate small 4 20000

printf '\n%-8s %-40s %12s %10s %10s %14s\n' dataset binary "startup ms" "read MiB" "peak MiB" "check ms/MiB"
for dataset in large small; do
  for bin in "$@"; do
    results=$(for _ in $(seq 1 "$RUNS"); do start_once "$bin" "$dataset"; done)
    ms=$(cut -d' ' -f1 <<<"$results" | median)
    read_mib=$(cut -d' ' -f2 <<<"$results" | median)
    peak=$(cut -d' ' -f3 <<<"$results" | median)
    check=-
    if "$bin" check-data-dir --help >/dev/null 2>&1; then
      /usr/bin/time -f '%e %M' "$bin" check-data-dir --data-dir "$WORK/$dataset" >/dev/null 2>"$WORK/time" || true
      read -r secs kb <"$WORK/time"
      check="$(awk -v s="$secs" 'BEGIN { printf "%d", s * 1000 }')/$(( kb / 1024 ))"
    fi
    printf '%-8s %-40s %12s %10s %10s %14s\n' "$dataset" "$bin" "$ms" "$read_mib" "$peak" "$check"
  done
done
rm -rf "$WORK"
