#!/usr/bin/env bash
# REL-18 transport regression budgets: three steady 60 s runs each of the
# baseline and the candidate server (scripts/bench/transport-budgets.md,
# "Regression budgets"), interleaved B C, C B, B C so that drift over the
# session affects both alike. Every run uses the same connload, from
# TARGET_VOLUME; only the server build changes.
#
#   scripts/bench/transport-regression.sh OUT BASELINE_VOLUME CANDIDATE_VOLUME
#   node scripts/bench/transport-regression-report.mjs OUT
#
# BASELINE_VOLUME and CANDIDATE_VOLUME each hold a release/vyrtel (the
# baseline volume needs no connload). Build them as in docs/testing.md
# ("Transport soak on Linux"), the baseline at the commit declared in
# transport-budgets.md.
#
# Environment: RUNS (per build, default 3), DURATION (connload --duration,
# default 60s), TARGET_VOLUME (connload; see transport-soak.sh), IMAGE, and
# the other transport-soak.sh variables, which apply to every run alike.
# BASELINE_SERVER_ENV and CANDIDATE_SERVER_ENV add "-e KEY=VALUE" flags for
# one build only: the declared baseline (0.1.1) has sign-in off by default
# and needs "-e VYRTEL_AUTH_ENABLED=true", which 0.1.2 refuses as a setting
# that no longer exists (sign-in is always on there).
# BASELINE_CONNLOAD_ARGS and CANDIDATE_CONNLOAD_ARGS add connload flags for
# one build only: 0.1.1 has no transport counters in system/info, so its
# runs need --no-transport-counters (the report allows it on baseline runs
# only; none of the regression metrics come from those counters). For the
# declared baseline:
#
#   BASELINE_SERVER_ENV="-e VYRTEL_AUTH_ENABLED=true" \
#   BASELINE_CONNLOAD_ARGS=--no-transport-counters \
#   scripts/bench/transport-regression.sh OUT rel18-baseline rel18-target
#
# Writes OUT/{baseline,candidate}-N/ (transport-soak.sh's files) and
# OUT/runs.txt (the order the runs were made in).
set -euo pipefail

[ $# -eq 3 ] || { echo "usage: $0 OUT BASELINE_VOLUME CANDIDATE_VOLUME" >&2; exit 2; }
out=$1
declare -A vol=([baseline]=$2 [candidate]=$3)
declare -A extra=([baseline]="${BASELINE_CONNLOAD_ARGS:-}" [candidate]="${CANDIDATE_CONNLOAD_ARGS:-}")
declare -A env=([baseline]="${SERVER_ENV:-} ${BASELINE_SERVER_ENV:-}" [candidate]="${SERVER_ENV:-} ${CANDIDATE_SERVER_ENV:-}")
runs=${RUNS:-3}
duration=${DURATION:-60s}
soak=$(dirname "$0")/transport-soak.sh

for b in baseline candidate; do
  docker volume inspect "${vol[$b]}" >/dev/null || { echo "no Docker volume ${vol[$b]} ($b)" >&2; exit 2; }
done
mkdir -p "$out"
: > "$out/runs.txt"

for i in $(seq 1 "$runs"); do
  if (( i % 2 )); then order="baseline candidate"; else order="candidate baseline"; fi
  for b in $order; do
    dir="$out/$b-$i"
    [ ! -e "$dir" ] || { echo "$dir exists; use a fresh OUT" >&2; exit 2; }
    echo "$(date -u +%FT%TZ) $b-$i ${vol[$b]} server env:${env[$b]} connload:${extra[$b]}" | tee -a "$out/runs.txt"
    # A failed run is kept and reported by the report, not retried here.
    SERVER_VOLUME=${vol[$b]} SERVER_ENV=${env[$b]} "$soak" "$dir" --no-waves --duration "$duration" ${extra[$b]} >/dev/null || \
      echo "$b-$i: transport-soak.sh exited $?" | tee -a "$out/runs.txt"
  done
done
