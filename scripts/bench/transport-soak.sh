#!/usr/bin/env bash
# REL-18 transport evidence on Linux: run a release `vyrtel` and `connload`
# in separate containers (so the load generator's CPU and memory are not the
# server's), sample the server process from /proc every second, then check
# that it returns to idle and stops in time.
#
#   scripts/bench/transport-soak.sh <out-dir> [connload arguments...]
#
# The binaries come from a Docker volume holding a release build
# (`cargo build --release -p server -p loadgen` with CARGO_TARGET_DIR on the
# volume; see docs/testing.md#transport-soak-on-linux).
#
# Environment (defaults in brackets):
#   TARGET_VOLUME   volume with release/vyrtel and release/connload [rel18-target]
#   SERVER_VOLUME   volume with the release/vyrtel to measure, e.g. a
#                   baseline build that has no connload          [TARGET_VOLUME]
#   IMAGE           runtime image with a shell and /proc            [debian:bookworm-slim]
#   SERVER_MEMORY   server container memory limit                    [1g]
#   SERVER_CPUS     server container CPU limit                       [2]
#   LOAD_CPUS       load generator CPU limit                         [2]
#   NOFILE          server open-file limit                           [4096]
#   SERVER_ENV      extra "-e KEY=VALUE" flags for the server        []
#   IDLE_AFTER      seconds sampled after the load ends              [30]
#
# cgroupKb is the server container's memory.current (process memory, page
# cache and kernel memory charged to it); socketBuffersKb is the `sock` line
# of its memory.stat: kernel TCP buffers of the server's sockets, which are
# not part of the process heap or RSS. listenOverflows is the kernel's
# count of handshakes dropped because the accept queue was full (the
# server's network namespace, cumulative).
#
# Writes <out-dir>/resources.csv (one row per second), server.log,
# summary.json (from connload), and resources.json (peaks, idle levels,
# CPU time, shutdown time and exit status).
set -euo pipefail

out=${1:?usage: transport-soak.sh <out-dir> [connload arguments...]}
shift
mkdir -p "$out"
out=$(cd "$out" && pwd)

volume=${TARGET_VOLUME:-rel18-target}
image=${IMAGE:-debian:bookworm-slim}
net=rel18-net-$$
server=rel18-server-$$
load=rel18-load-$$

cleanup() {
  docker rm -f "$server" "$load" >/dev/null 2>&1 || true
  docker network rm "$net" >/dev/null 2>&1 || true
}
trap cleanup EXIT

docker network create "$net" >/dev/null
# shellcheck disable=SC2086 # SERVER_ENV is a list of flags
docker run -d --name "$server" --network "$net" \
  --memory "${SERVER_MEMORY:-1g}" --cpus "${SERVER_CPUS:-2}" --ulimit "nofile=${NOFILE:-4096}" \
  -v "${SERVER_VOLUME:-$volume}:/target:ro" -e VYRTEL_ALERTS_ENABLED=false ${SERVER_ENV:-} \
  "$image" /target/release/vyrtel --data-dir /data --bind 0.0.0.0:8080 >/dev/null

# Port 8080 is 1F90 in /proc/net/tcp. For the listening socket, rx_queue is
# the accept backlog right now; established sockets to the port include
# those still in that backlog.
probe='
s=/proc/1/status; read -r _ _ _ _ _ _ _ _ _ _ _ _ _ ut st _ < /proc/1/stat
rss=$(awk "/VmRSS/{print \$2}" $s); hwm=$(awk "/VmHWM/{print \$2}" $s); thr=$(awk "/Threads/{print \$2}" $s)
fds=$(ls /proc/1/fd | wc -l); socks=$(ls -l /proc/1/fd 2>/dev/null | grep -c socket || true)
hex=$(awk "NR>1 && \$2 ~ /:1F90$/ && \$4==\"0A\" {split(\$5,q,\":\"); print q[2]}" /proc/net/tcp | head -1)
est=$(awk "NR>1 && \$2 ~ /:1F90$/ && \$4==\"01\"" /proc/net/tcp | wc -l)
cg=$(cat /sys/fs/cgroup/memory.current 2>/dev/null || echo 0)
sock=$(awk "\$1==\"sock\" {print \$2}" /sys/fs/cgroup/memory.stat 2>/dev/null)
ovf=$(awk "/^TcpExt:/{if(!h){split(\$0,k);h=1}else{split(\$0,v);for(i=1;i<=NF;i++) if(k[i]==\"ListenOverflows\") print v[i]}}" /proc/net/netstat)
echo "$rss,$hwm,$thr,$fds,$socks,$ut,$st,$((0x${hex:-0})),$est,$((cg / 1024)),$(( ${sock:-0} / 1024 )),${ovf:-0}"'

sample() {
  local start=$1
  while docker exec "$server" true 2>/dev/null; do
    row=$(docker exec "$server" sh -c "$probe" 2>/dev/null) || break
    echo "$(( $(date +%s%3N) - start )),$row" >> "$out/resources.csv"
    sleep 1
  done
}

for _ in $(seq 1 50); do
  docker exec "$server" sh -c 'awk "NR>1 && \$2 ~ /:1F90$/ && \$4==\"0A\"" /proc/net/tcp | grep -q .' && break
  sleep 0.2
done

ticks=$(docker exec "$server" getconf CLK_TCK)
# Docker Desktop on Windows (Git Bash) needs a Windows path to mount.
mount_out=$out
if command -v cygpath >/dev/null 2>&1; then mount_out=$(cygpath -m "$out"); fi

echo "elapsedMs,rssKb,hwmKb,threads,fds,sockets,utimeTicks,stimeTicks,listenBacklog,established,cgroupKb,socketBuffersKb,listenOverflows" > "$out/resources.csv"
start=$(date +%s%3N)
sleep 5 # idle level before load
sample "$start" &
sampler=$!

load_status=0
docker run --name "$load" --network "$net" --cpus "${LOAD_CPUS:-2}" --ulimit nofile=65536 \
  -v "$volume:/target:ro" -v "$mount_out:/out" "$image" \
  /target/release/connload --target "http://$server:8080" --out /out "$@" > "$out/connload.log" 2>&1 || load_status=$?
load_end=$(( $(date +%s%3N) - start ))

sleep "${IDLE_AFTER:-30}"
stop_begin=$(date +%s%3N)
docker stop -t 30 "$server" >/dev/null
stop_ms=$(( $(date +%s%3N) - stop_begin ))
exit_code=$(docker inspect -f '{{.State.ExitCode}}' "$server")
wait "$sampler" 2>/dev/null || true
docker logs "$server" > "$out/server.log" 2>&1

awk -F, -v load_end="$load_end" -v stop_ms="$stop_ms" -v exit_code="$exit_code" -v ticks="$ticks" \
  -v load_status="$load_status" '
  NR == 1 { next }
  NR == 2 { idle_before = $2 }
  {
    rows++; last = $0
    if ($2 > rss) rss = $2; if ($4 > thr) thr = $4; if ($5 > fds) fds = $5; if ($6 > socks) socks = $6
    if ($9 > backlog) backlog = $9; if ($10 > est) est = $10; if ($11 > cg) cg = $11; if ($12 > sockmem) sockmem = $12; if ($13 > ovf) ovf = $13
    cpu = ($7 + $8) / ticks
    if ($1 > load_end) { after[++n] = $2; after_socks = $6; after_fds = $5 }
  }
  END {
    idle_after = n ? after[n] : 0
    printf "{\n  \"samples\": %d,\n  \"rssKb\": {\"idleBefore\": %d, \"peak\": %d, \"idleAfter\": %d},\n", rows, idle_before, rss, idle_after
    printf "  \"peakThreads\": %d,\n  \"peakFds\": %d,\n  \"peakSockets\": %d,\n  \"socketsAfter\": %d,\n  \"fdsAfter\": %d,\n", thr, fds, socks, after_socks, after_fds
    printf "  \"peakListenBacklog\": %d,\n  \"peakEstablishedToPort\": %d,\n  \"cpuSeconds\": %.2f,\n", backlog, est, cpu
    printf "  \"peakCgroupKb\": %d,\n  \"peakSocketBuffersKb\": %d,\n  \"listenOverflows\": %d,\n", cg, sockmem, ovf
    printf "  \"loadEndedAtMs\": %d,\n  \"connloadExit\": %d,\n  \"shutdownMs\": %d,\n  \"serverExit\": %d\n}\n", load_end, load_status, stop_ms, exit_code
  }' "$out/resources.csv" > "$out/resources.json"
cat "$out/resources.json"
exit "$load_status"
