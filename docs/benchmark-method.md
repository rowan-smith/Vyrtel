# Benchmark method

This is how the numbers on the [Benchmarks](benchmarks.md) page are produced. It is one process in
four stages, run by hand for each release on dedicated hardware: the **primary** server runs
Vyrtel, and a **secondary** machine, used only in the last stage, generates load. Do not set up the
secondary until every primary-only test has passed.

| Stage | Where | Outcome |
|---|---|---|
| [1. Setup and smoke tests](#stage-1-primary-setup-and-smoke-tests) | Primary | Firewall, install, UI and API smoke tests |
| [2. Primary-only tests](#stage-2-primary-only-tests) | Primary | Correctness, five-run baseline, transport stress, recovery |
| [3. Remote load and soak](#stage-3-remote-load-and-soak) | Secondary drives, primary measures | Remote ingest ramp and a 48–72 hour soak |
| [4. Sign-off and publish](#stage-4-sign-off-and-publish) | — | Evidence review, tag, published-artifact check, report |

For a comparison between versions to mean anything, keep the primary hardware, configuration, input
data, seed and build provenance the same for every version measured.

## Kinds of result

Each kind of result is reported separately and never mixed with another:

| Kind | What it measures |
|---|---|
| Local baseline | The [baseline harness](../dotnet/Vyrtel.Baseline/README.md) on the primary, client and server on one host |
| Same-host transport | Server and traffic driver in two containers on the primary |
| Remote, direct | Load from the secondary over a private network |
| Remote, SSH tunnel | Load from the secondary through an SSH tunnel. Limited by the tunnel, so not a network benchmark |

Single-host results share CPU between client and server; label them local, not remote.

## Verdicts

- **PASS**: every applicable gate ran and met its declared limits.
- **FAIL**: a correctness check or a budget failed.
- **INCOMPLETE**: required evidence is missing or invalid, for example a tool that could not
  authenticate, lost samples or an unreachable network.

A test that never ran is never reported as passing. Budgets are declared before the run and are not
loosened, nor the workload changed, to hide a regression.

## Hosts

Ubuntu Linux x86-64 for both machines. Record the actual CPU, memory, disks and filesystem of each
host rather than relying on a provider's listing.

Vyrtel listens only on `127.0.0.1:8080` and is reached through an SSH tunnel; `firewalld` allows only
SSH. In stage 3 a single firewall rule lets the secondary's private address reach port 8080. Never
expose Vyrtel, its admin password or API keys on a public port 8080.

## Stage 1: Primary setup and smoke tests

### Firewall

Keep your first SSH session open and make sure the provider offers a recovery console before
changing firewall rules. If `ufw` is active (`sudo ufw status`), migrate from it rather than running
two firewalls. If SSH uses a non-standard port, allow that port instead of the `ssh` service.

```bash
sudo apt-get update
sudo apt-get install -y firewalld curl git jq docker.io sysstat
sudo firewall-offline-cmd --zone=public --add-service=ssh
sudo systemctl enable --now firewalld
sudo systemctl enable --now docker
sudo firewall-cmd --get-active-zones
sudo firewall-cmd --list-all
```

Check that the zone actually active on the SSH interface allows SSH and does **not** open
`8080/tcp`, then open a second SSH session before closing the first.

### Record the host

```bash
export TAG='v0.1.1'   # the candidate or release under test
export RUN_ID="$(date -u +%Y%m%dT%H%M%SZ)"
export RUN_ROOT="$HOME/vyrtel-tests/$TAG/$RUN_ID"
mkdir -p "$RUN_ROOT"/{host,install,smoke,baseline,local,remote,soak}
lscpu > "$RUN_ROOT/host/cpu.txt"
free -h > "$RUN_ROOT/host/ram.txt"
lsblk -o NAME,MODEL,SIZE,ROTA,TYPE,FSTYPE,MOUNTPOINTS > "$RUN_ROOT/host/disks.txt"
df -hT > "$RUN_ROOT/host/filesystems.txt"
cat /etc/os-release > "$RUN_ROOT/host/os.txt"
sudo firewall-cmd --list-all > "$RUN_ROOT/host/firewall.txt"
```

Keep `RUN_ROOT` for later sessions.

### Install

Choose **one** of these; never run two on port 8080 at once.

**Docker image** of a published tag:

```bash
export IMAGE="ghcr.io/rowan-smith/vyrtel:${TAG#v}"
sudo docker pull "$IMAGE"
sudo docker volume create vyrtel-test-data
sudo docker run -d --name vyrtel-test \
  -p 127.0.0.1:8080:8080 \
  -v vyrtel-test-data:/data --stop-timeout 15 "$IMAGE"
sudo docker logs vyrtel-test > "$RUN_ROOT/install/first-start.log" 2>&1
curl -fsS http://127.0.0.1:8080/ready
```

The `127.0.0.1:` in the port mapping is deliberate. Docker writes its own firewall rules, so a plain
`-p 8080:8080` is not protected by the host firewall.

**Release binary** of a published tag:

```bash
mkdir -p "$RUN_ROOT/install/binary" "$RUN_ROOT/install/data"
cd "$RUN_ROOT/install/binary"
BASE="https://github.com/rowan-smith/Vyrtel/releases/download/$TAG"
curl -fL -O "$BASE/vyrtel-linux-x86_64"
curl -fL -O "$BASE/SHA256SUMS"
sha256sum --check --ignore-missing SHA256SUMS
chmod +x vyrtel-linux-x86_64
nohup ./vyrtel-linux-x86_64 --data-dir "$RUN_ROOT/install/data" \
  --bind 127.0.0.1:8080 > "$RUN_ROOT/install/server.log" 2>&1 &
echo "$!" > "$RUN_ROOT/install/server.pid"
curl -fsS http://127.0.0.1:8080/ready
```

**Untagged candidate**: check out the exact candidate commit, build it with the embedded UI as in
the [development guide](development.md), and run `target/release/vyrtel --data-dir ... --bind
127.0.0.1:8080`. Record `git rev-parse HEAD` and `sha256sum target/release/vyrtel`.

### Smoke tests

Open the UI from your own computer through a tunnel, then browse to `http://localhost:8080` and sign
in as `admin` with the password from the first-start log. That log holds a credential; never publish
it unredacted.

```bash
ssh -N -L 8080:127.0.0.1:8080 <user>@<primary-ssh-ip>
```

On the primary:

```bash
curl -i http://127.0.0.1:8080/health
curl -i http://127.0.0.1:8080/ready
curl -i -X POST http://127.0.0.1:8080/api/v1/events \
  -H 'content-type: application/json' \
  -d '{"level":"Information","service":"release-smoke","message":"first release smoke event","properties":{"run":1}}'
```

- [ ] The smoke event appears in search for a time range covering now, with its properties.
- [ ] A wrong password is refused; the admin login works; the UI and settings load.
- [ ] With an ingest key created, anonymous ingest is refused and authenticated ingest succeeds.
- [ ] Revoking the last key leaves ingestion as described in [Security](guide/security.md).
- [ ] After a stop and restart on the same volume or directory, the event is still there, `/ready`
      answers, and the process stops cleanly.

**Stop** if startup, login, ingest, search, key handling or persistence fails, and keep the
evidence. Benchmarks against a broken build are not run.

## Stage 2: Primary-only tests

The secondary stays idle throughout this stage.

### Correctness and reliability

- [ ] Structured and plain events ingest and are searchable.
- [ ] Queries, filters, OpenTelemetry signals and the core UI pages work.
- [ ] Sessions, keys and permissions behave as documented.
- [ ] A graceful restart keeps every acknowledged event.
- [ ] The crash-recovery, backup and restore, and release gate tests in [Testing](testing.md) pass;
      keep their exit codes and logs.

### Five-run baseline

The baseline measures the exact candidate against the previous approved release on the same machine,
with the same `baseline.toml`, harness and parameters, and five runs per version. It needs the
repository checkout with Rust, .NET and Node 22+. The harness starts its own server with clean data
directories, so stop the manual instance first (`sudo docker stop vyrtel-test`) and make sure no
other load is running.

From the candidate checkout, with the candidate built to `target/release/vyrtel`:

```bash
export PREVIOUS_TAG=v0.1.1   # the previous approved release
mkdir -p "$RUN_ROOT/baseline/prior-bin" "$RUN_ROOT/baseline/prior" "$RUN_ROOT/baseline/candidate"
BASE="https://github.com/rowan-smith/Vyrtel/releases/download/$PREVIOUS_TAG"
curl -fL -o "$RUN_ROOT/baseline/prior-bin/vyrtel-linux-x86_64" "$BASE/vyrtel-linux-x86_64"
curl -fL -o "$RUN_ROOT/baseline/prior-bin/SHA256SUMS" "$BASE/SHA256SUMS"
(cd "$RUN_ROOT/baseline/prior-bin" && sha256sum -c --ignore-missing SHA256SUMS)
chmod +x "$RUN_ROOT/baseline/prior-bin/vyrtel-linux-x86_64"

dotnet run --project dotnet/Vyrtel.Baseline -c Release -- \
  --runs 5 --label "$PREVIOUS_TAG" \
  --server "$RUN_ROOT/baseline/prior-bin/vyrtel-linux-x86_64" \
  --out "$RUN_ROOT/baseline/prior/report.json"

dotnet run --project dotnet/Vyrtel.Baseline -c Release -- \
  --runs 5 --label "$TAG" \
  --server ./target/release/vyrtel \
  --out "$RUN_ROOT/baseline/candidate/report.json" \
  --compare "$RUN_ROOT/baseline/prior/report.json"
```

Each run writes `report.json`, `report.md` and `report.samples.csv`. The comparison exits with `2`
when the candidate does not pass. A re-check of an already published tag uses its published binary
and is labelled post-release verification, not the pre-tag sign-off.

### Same-host transport

The [transport soak](../scripts/bench/transport-soak.sh) runs the server and a traffic driver in
separate containers on the primary. It is a single-host test, not a two-machine benchmark.

```bash
sudo docker volume create rel18-target
sudo docker run --rm -v "$PWD:/src:ro" -v rel18-target:/target \
  -e CARGO_TARGET_DIR=/target -w /src rust:1.96-bookworm \
  cargo build --release --locked -p server -p loadgen

sudo env IMAGE=rust:1.96-bookworm scripts/bench/transport-soak.sh \
  "$RUN_ROOT/local/steady-1" --no-waves --duration 60s
sudo env IMAGE=rust:1.96-bookworm scripts/bench/transport-soak.sh \
  "$RUN_ROOT/local/flood-1" --duration 180s --attempts 1000
sudo env IMAGE=rust:1.96-bookworm scripts/bench/transport-soak.sh \
  "$RUN_ROOT/local/transport-30m" --duration 30m --wave-interval 60s
sudo chown -R "$USER:$(id -gn)" "$RUN_ROOT/local"
```

Keep `resources.csv`, `resources.json`, `summary.json` and the logs, and run the extra repeats and
the constrained case that [the transport budgets](../scripts/bench/transport-budgets.md) require.

### Recovery

Restart the stage 1 install on its unchanged volume or directory and check search, readiness and
shutdown. Run the backup, restore and crash-recovery drills on test data only.

**Stop** if a functional gate fails, events go missing, the baseline comparison fails, or transport
or recovery evidence is missing.

## Stage 3: Remote load and soak

The load generator moves to its own machine so its CPU and memory do not distort the server's
measurements.

### Set up the secondary

```bash
sudo apt-get update
sudo apt-get install -y firewalld git curl jq build-essential pkg-config libssl-dev sysstat tmux
sudo firewall-offline-cmd --zone=public --add-service=ssh
sudo systemctl enable --now firewalld
sudo firewall-cmd --get-active-zones
sudo firewall-cmd --list-all
```

Apply the same SSH care as on the primary. The secondary needs no incoming port 8080. Record its
`lscpu`, `free -h`, `lsblk`, OS, kernel and network details.

### Connect the machines

**Private network (preferred).** On the primary, allow only the secondary's private address to reach
port 8080, in the zone attached to the private interface:

```bash
export PRIMARY_PRIVATE_IP='<primary-private-ip>'
export SECONDARY_PRIVATE_IP='<secondary-private-ip>'
export PRIVATE_ZONE='public'   # the private interface's zone, from --get-active-zones
sudo firewall-cmd --permanent --zone="$PRIVATE_ZONE" \
  --add-rich-rule="rule family=ipv4 source address=${SECONDARY_PRIVATE_IP}/32 port port=8080 protocol=tcp accept"
sudo firewall-cmd --reload
sudo firewall-cmd --zone="$PRIVATE_ZONE" --list-rich-rules
```

Never add a broad `--add-port=8080/tcp` to a public zone. If the provider's "private" network is
routed over the internet or untrusted, use TLS or a VPN instead.

Then run Vyrtel with host networking, bound to the private address. Host networking makes the host
firewall apply, which Docker's published ports bypass:

```bash
sudo docker stop vyrtel-test && sudo docker rm vyrtel-test
sudo docker run -d --name vyrtel-test --network host \
  -e VYRTEL_SERVER_BIND="${PRIMARY_PRIVATE_IP}:8080" \
  -v vyrtel-test-data:/data --stop-timeout 15 "$IMAGE"
curl -fsS "http://${PRIMARY_PRIVATE_IP}:8080/ready"
```

Check with `ss -lntp` that it listens only on that address, and from an unrelated host that port
8080 is not reachable. For an untagged candidate, bind its binary to the same address instead.

**No private network.** Keep Vyrtel on `127.0.0.1` as in stage 1 and tunnel from the secondary,
kept alive in `tmux`. Results taken this way are reported as SSH-tunnel results.

```bash
ssh -N -L 18080:127.0.0.1:8080 <user>@<primary-ssh-ip>
```

### Build the load generator

Build it on the secondary from the exact candidate commit, or the exact published tag, with a Rust
toolchain the lockfile accepts:

```bash
git clone https://github.com/rowan-smith/Vyrtel.git "$HOME/vyrtel-loadgen"
cd "$HOME/vyrtel-loadgen"
git checkout --detach '<candidate-commit-or-tag>'
cargo build --release --locked -p loadgen
sha256sum target/release/loadgen

export VYRTEL_URL='http://<primary-private-ip>:8080'   # or http://127.0.0.1:18080 through the tunnel
export RUN_ID='<same run id as the primary>'
export OUT="$HOME/vyrtel-tests/remote/$RUN_ID"
mkdir -p "$OUT"
curl -fsS "$VYRTEL_URL/ready"
```

### Remote ingest ramp

Every trial starts from a fresh, empty volume, so no trial inherits another's data. On the
**primary**, for each `RATE` and `REPEAT`:

```bash
export RATE=1000 REPEAT=1
sudo docker stop vyrtel-test && sudo docker rm vyrtel-test
sudo docker volume create "vyrtel-load-${RUN_ID}-${RATE}-${REPEAT}"
sudo docker run -d --name vyrtel-test --network host \
  -e VYRTEL_SERVER_BIND="${PRIMARY_PRIVATE_IP}:8080" \
  -v "vyrtel-load-${RUN_ID}-${RATE}-${REPEAT}:/data" \
  --stop-timeout 15 "$IMAGE"
curl -fsS "http://${PRIMARY_PRIVATE_IP}:8080/ready"
```

Through a tunnel, use the stage 1 loopback mapping instead of host networking.

Then on the **secondary**, with the same values:

```bash
export RATE=1000 REPEAT=1
./target/release/loadgen --target "$VYRTEL_URL" \
  --rate "$RATE" --duration 60s --seed 42 \
  --services 8 --error-rate 0.02 --cardinality 10000 \
  --traces-every 200 \
  > "$OUT/ramp-${RATE}-run${REPEAT}.log" 2>&1
```

Run rates 1,000, 5,000 and 10,000 events/s, three repeats each: nine trials. These are offered
rates, not achieved throughput. If ingestion needs a key, pass an ingest-scoped key in
`VYRTEL_API_KEY` and keep it out of logs.

While each trial runs, record the primary's CPU, memory, disk I/O, sockets, stored events and
errors, and the secondary's CPU and network use. After each trial check that:

- accepted events are searchable and reconcile with what the generator reports;
- there are no unexpected transport errors;
- the primary stayed within its declared limits;
- the secondary's CPU or network was not the bottleneck.

### Mixed query load

Query and system endpoints need an authenticated session or admin key; producers use separate
ingest keys. Never turn off sign-in to make a benchmark pass. The connection-load tool does not yet
authenticate its query calls, so mixed-query results are reported as INCOMPLETE until it does. An
ingest-only ramp can still be run and reported as such.

### Soak (48–72 hours)

Start a fresh, persistent instance on the primary and keep a resource sampler running: CPU, memory,
open files and sockets, disk use and ingestion backpressure, with timestamps. On the secondary, run
a steady load inside `tmux` or a systemd unit so a dropped connection does not end it:

```bash
mkdir -p "$OUT/soak"
tmux new -s vyrtel-soak
./target/release/loadgen --target "$VYRTEL_URL" \
  --rate 40 --duration 48h --seed 42 \
  --services 8 --error-rate 0.02 --cardinality 10000 \
  --traces-every 200 > "$OUT/soak/loadgen-48h.log" 2>&1
```

The rate is chosen in advance to suit the disk; 40 events/s is an example. Record the generator's
exit status rather than taking a log file as success.

Afterwards, stop the load, record ten minutes of idle recovery, stop the primary gracefully, restart
it on the same volume and check that every acknowledged event is still there, then run the backup,
restore and recovery drill. Report the memory trend, p95 and p99 latency, disk bytes, errors, CPU,
and which side was the bottleneck.

The soak passes only if both machines' evidence is complete, acknowledged events reconcile with
stored events, nothing ran out of memory or grew without bound, and the declared limits were met.

### Close the port

On the primary, remove the rule once testing is over, and stop Vyrtel or return it to the loopback
mapping:

```bash
sudo firewall-cmd --permanent --zone="$PRIVATE_ZONE" \
  --remove-rich-rule="rule family=ipv4 source address=${SECONDARY_PRIVATE_IP}/32 port port=8080 protocol=tcp accept"
sudo firewall-cmd --reload
```

## Stage 4: Sign-off and publish

### Evidence

Collect both machines' results in one place, with passwords and keys left out:

- the primary's hardware, OS, firewall, and the binary hash or image digest;
- the five-run baseline reports and comparison result;
- same-host transport, reliability and recovery results;
- the secondary's hardware, load generator commit and hash, and the remote results;
- the soak's resource time series, offered, acknowledged and stored counts, client errors and
  restart evidence;
- the declared budgets, the verdict for each, limitations and anything skipped.

### Tag

For a stable release, performance is measured on the exact commit that will be tagged, after its
compatibility review, and the evidence is committed before the tag is created. [Releasing](releasing.md)
describes the gates, their order, and how prereleases differ. Any change after sign-off means
reviewing and measuring again.

### Check the published artifacts

Once the release workflow is green, repeat the stage 1 install and smoke tests on a fresh volume
using the exact published tag, never `latest`, and verify `SHA256SUMS`, the image digest and version,
and the release links. This is a separate check, not a substitute for the pre-tag gate.

### Publish

The results go on the [Benchmarks](benchmarks.md) page, with local and remote numbers in separate
rows, SSH-tunnel results marked as such, and every untested area stated. Results are not compared
across versions when the hardware, configuration or workload changed materially.

Final checklist:

- [ ] Candidate commit, artifact hashes, platform, configuration and both hosts are documented.
- [ ] Correctness, recovery and security tests pass.
- [ ] No unexplained loss of acknowledged events or uncontrolled resource growth.
- [ ] The local baseline and any remote benchmark meet their declared budgets.
- [ ] The compatibility and performance sign-offs are for the exact commit, in the right order.
- [ ] The published binary or image was verified separately.
- [ ] The published page has real results and honest caveats.
