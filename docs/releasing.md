# Releasing

Releases are published by [`.github/workflows/release.yml`](../.github/workflows/release.yml)
when a `v*` tag is pushed. The workflow publishes only after the release has been validated:
if any check fails, no binaries, image or GitHub release are published.

Validation starts before the tag exists. Every stable version has performance gates that a
reviewer signs off for the exact candidate commit before it is tagged
([Pre-tag performance sign-off](#pre-tag-performance-sign-off)). The supported way to create a
tag refuses without that sign-off, and the release workflow independently refuses to publish a
tag whose sign-off is missing, failed, stale or recorded after tagging.

## What gets published

| Artifact                                                                | Where                        | Stable tag (`v0.1.2`)    | Prerelease tag (`v0.1.2-rc.1`)       |
|-------------------------------------------------------------------------|------------------------------|--------------------------|--------------------------------------|
| Linux x86_64/ARM64, macOS ARM64, Windows x86_64 binaries + `SHA256SUMS` | GitHub release               | marked **Latest**        | marked **Pre-release**, never Latest |
| Container image (`linux/amd64`, `linux/arm64`)                          | `ghcr.io/rowan-smith/vyrtel` | `0.1.2`, `0.1`, `latest` | `0.1.2-rc.1` only                    |

A prerelease never moves `latest` or `<major>.<minor>`, on GHCR or GitHub, so
`docker pull ghcr.io/rowan-smith/vyrtel:latest` and the "latest release" download links
always point at the newest stable release.

## Gates

The jobs run in this order. A failed job skips every job after it.

| Job         | Checks                                                                                                                                                                                                                                                                                                                          | Fails when                                                                                |
|-------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|-------------------------------------------------------------------------------------------|
| `preflight` | The tag is `v<major>.<minor>.<patch>[-<prerelease>]` and its version equals the `[workspace.package]` version in `Cargo.toml`, the `server` entry in `Cargo.lock` and `web/package.json`. Works out the channel (stable or prerelease) and the image tags.                                                                      | The tag is malformed, or any of the three versions differs from the tag.                  |
| `performance` | Every performance gate declared for the version in [`performance-gates.json`](../scripts/release/performance-gates.json) has a valid reviewer PASS on the `release-evidence` branch for this exact commit, all gates name the same candidate, and every review predates the (annotated) tag. | A record is missing, failed, incomplete, stale (another commit) or split across commits; it was reviewed after tagging; the tag is lightweight; the version declares no gates. |
| `ci`        | The whole [CI workflow](../.github/workflows/ci.yml) against the tagged commit: fmt, clippy, Rust tests, the restore drill, frontend lint/types/tests, Playwright E2E, the Docker [image checks](#container-image-checks) on `linux/amd64`, dependency checks, .NET build and the release gate tests.                                                                                                | Any CI job fails.                                                                         |
| `binaries`  | Builds each target with the UI that passed CI and `VYRTEL_REQUIRE_WEB_UI=1`, so a missing UI is a build error. On its own runner, each binary must print `vyrtel <version>` and, started on an empty data directory, report that version from `/api/v1/system/info` and serve the built UI and its `/assets/*.js` entry script. | Build fails; wrong version; placeholder UI; missing UI assets; the server does not start. |
| `verify`    | Combines the per-target checksums into `SHA256SUMS`, then checks the artifact set is exactly the four binaries, none is empty, and every checksum matches. Re-runs the binary check on the Linux x86_64 binary as downloaded. Checks the built artifacts have the hashes the performance gates profiled, or that the record documents reproducible equivalence. | A binary is missing, empty, unexpected or does not match its checksum; a profiled artifact's hash differs with no documented equivalence. |
| `image-smoke`| On native `linux/amd64` and `linux/arm64` runners, builds the image with the release version and runs every [image check](#container-image-checks): non-root user and version label, `--version`, readiness and the embedded UI on a named volume, events surviving a recreated container, and the uid hint on an unwritable bind mount. | Any image check fails on either architecture.                                             |
| `image`     | Only after both smoke tests pass, pushes the multi-architecture image with the preflight image tags, then inspects it in the registry: both platforms present, each running as `nonroot` with the release version label.                                                                                                        | The push fails, or the pushed image lacks a platform, the user or the label.              |
| `release`   | Re-checks `SHA256SUMS` and creates the GitHub release from the verified artifacts, as a prerelease (not Latest) for prerelease tags.                                                                                                                                                                                            | Checksums no longer match.                                                                |

The checks are in [`scripts/release/release.mjs`](../scripts/release/release.mjs) (Node 22, no
dependencies) so they can be run locally exactly as the workflow runs them. The performance
checks are in [`scripts/release/performance.mjs`](../scripts/release/performance.mjs), the image
checks in [`scripts/release/image.mjs`](../scripts/release/image.mjs).

## Container image checks

`verify-image` runs a built image the way an operator deploys it and checks each acceptance
criterion of REL-09 ([#10](https://github.com/rowan-smith/Vyrtel/issues/10)). It needs Docker and
Node 22 on the host, and removes every container and volume it creates, pass or fail.

| Step       | Checks                                                                                                                                                                                                                                                              |
|------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| config     | `docker image inspect`: the user is `nonroot:nonroot` (uid/gid 65532), `org.opencontainers.image.version` is the release version, the platform is the one asked for, `/data` is a volume, `VYRTEL_STORAGE_PATH=/data`, and the `vyrtel healthcheck` HEALTHCHECK is set. |
| version    | `docker run <image> --version` prints `vyrtel <version>`.                                                                                                                                                                                                            |
| serve      | On a new named volume, `/health` answers, `/api/v1/system/info` reports the version, `/` serves the built UI and its entry script, `vyrtel healthcheck` passes inside the container, and `docker top` shows the server process as uid 65532.                       |
| persist    | Three events are ingested and queried back; the container is stopped (it must exit 0), removed and recreated on the same volume; the query returns the same events with the same ids and timestamps, and `dataDir` and `durability` are unchanged.                     |
| bind mount | A host directory owned by the runner's user with mode `555` is mounted at `/data`: the server must exit non-zero with the [uid hint](configuration.md#docker-and-bind-mounts). With root or passwordless `sudo`, the directory is then given to 65532 as documented and the server must start, answer and stop cleanly. |

`--bind-mount auto` (the default) runs the last step on Linux hosts only: Docker Desktop on macOS
and Windows does not enforce host ownership on bind mounts, so the refusal cannot happen there.
The release and CI workflows pass `--bind-mount on`.

```bash
docker build --build-arg VYRTEL_VERSION=0.1.2 -t vyrtel:rc .
node scripts/release/release.mjs verify-image vyrtel:rc --version 0.1.2 --platform linux/amd64 --bind-mount on
```

```text
config: vyrtel:rc is linux/amd64, user nonroot:nonroot, org.opencontainers.image.version=0.1.2
version: --version printed "vyrtel 0.1.2"
serve: ready on a new volume, version 0.1.2, UI /assets/index-….js, healthcheck ok, process uid 65532
persist: 3 events (ids 0000000000000001, 0000000000000002, 0000000000000003), dataDir /data and durability normal unchanged after recreating the container on volume vyrtel-smoke-…-data
bind mount: unwritable /tmp/vyrtel-bind-… refused with the uid hint (vyrtel: locking the data directory: I/O error on /data/.lock: Permission denied (os error 13): …)
bind mount: after chown 65532:65532 the server starts and stops cleanly
vyrtel:rc: every image check passed for version 0.1.2
```

An image built without `--build-arg VYRTEL_VERSION` is labelled `dev` and fails the config
step. That is intended: the label is what operators and registries read. After a push,
`verify-pushed-image <ref>` runs the config step against each platform in the registry
(`docker buildx imagetools inspect`, no pull).

## Pre-tag performance sign-off

Performance is validated on the final candidate, after all of a milestone's work and fixes are
merged and **before** the tag is created. A green post-tag workflow does not replace it.

**Who:** the maintainer cutting the release owns the procedure. Each gate's issue (the
`PERF-GATE-*` tickets, e.g. [#170](https://github.com/rowan-smith/Vyrtel/issues/170) for 0.1.2)
owns the profiling, and a reviewer records the verdict.

**Which gates:** [`scripts/release/performance-gates.json`](../scripts/release/performance-gates.json)
lists the gates for each version. Milestones that share a version each have a gate, and all of
them must sign off the **same** commit (0.2.0 needs PERF-GATE-3 and PERF-GATE-4; 0.2.1 needs
PERF-GATE-5 and PERF-GATE-10). A stable version with no entry cannot be tagged; an entry may have
an empty list only with a `reason`. Prereleases need no sign-off (`"prereleases":
"not-required"`). A new delivery milestone gets its gate by adding its `PERF-GATE-*` issue to the
manifest in a reviewed pull request. Future and discovery scope gets no entry until it is
assigned to a delivery milestone.

**Where the evidence lives:** one approval record per gate on the `release-evidence` branch, at
`performance/<version>/<gate id>.json`. The record cannot live in the candidate commit: adding
it would change the commit it approves. Raw measurements and profiles are linked from `reports`.
Protect the branch so only maintainers can push to it.

**What a record holds.** The template is
[`testdata/performance/0.1.2/PERF-GATE-1.json`](../scripts/release/testdata/performance/0.1.2/PERF-GATE-1.json),
the same fixture the tests use; its values are illustrative, not measurements.

| Field                                                    | Rule                                                                                                                                                                                                                                                                                                                                         |
|----------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `gate`, `issue`, `version`                               | Match the manifest entry and the file path.                                                                                                                                                                                                                                                                                                  |
| `candidate.sha`                                          | The full commit SHA that was profiled and will be tagged.                                                                                                                                                                                                                                                                                    |
| `candidate.artifacts`                                    | Name and SHA-256 of each release-profile artifact that was profiled (at least one).                                                                                                                                                                                                                                                          |
| `baseline`                                               | The prior approved release and its commit SHA.                                                                                                                                                                                                                                                                                               |
| `budgetsDeclaredAt` ≤ `measuredAt` ≤ `review.reviewedAt` | Budgets are fixed before the candidate is measured; the review follows the measurements.                                                                                                                                                                                                                                                     |
| `review`                                                 | `verdict: "PASS"`, the reviewer's GitHub login and the time.                                                                                                                                                                                                                                                                                 |
| `reports`                                                | Links to the profiling reports, raw time series and CPU/allocation profiles.                                                                                                                                                                                                                                                                 |
| `metrics`                                                | At least one row per category: `footprint`, `cpu`, `ingestion`, `queries`, `correctness`, `backpressure`, `storage`, `networking`, `ui`, `lifecycle`. A measured row has `name`, `unit`, numeric `value`, `limit.max` and/or `limit.min`, `result`, and either `baseline` + `delta` (= value − baseline) or a `noBaseline` reason for an absolute budget. A category that does not apply has a `{ "category": …, "na": "<reason>" }` row. |
| `equivalence` (optional)                                 | `method` and `evidence` link, for when the release workflow's rebuilt artifacts are not byte-identical to the profiled ones.                                                                                                                                                                                                                 |

The check recomputes every row. A value outside its limit fails even when it is marked `pass`,
any `fail` row fails the gate, and a delta that does not match its baseline is rejected. Limits
are never loosened to pass: a regression is fixed and the affected workloads re-run.

**Invalidation:** the sign-off names one commit. Any later change to code, configuration, build
or dependencies makes a new commit, so the record no longer matches (`stale: approved candidate
… but the release commit is …`). Fix it, re-run the affected workloads plus the core mixed load,
and record a new PASS for the new commit.

**Artifact equivalence:** the release workflow rebuilds the binaries from the tagged commit. In
`verify`, each profiled artifact's hash must match the rebuilt one, or the record's
`equivalence` must explain why they are equivalent (for example the same commit, toolchain and
lock file). Either way, the rebuilt binaries have passed the version and embedded-UI smoke checks.

Check a candidate's sign-off at any time. With the template record (where your candidate's SHA
replaces the placeholder):

```bash
node scripts/release/release.mjs pretag --tag v0.1.2
```

```text
Vyrtel 0.1.2

Performance-Gate: PERF-GATE-1 PASS 1111111111111111111111111111111111111111 by example-reviewer at 2026-10-03T09:00:00Z

v0.1.2 may be tagged on 1111111111111111111111111111111111111111; rerun with --create to create the tag
```

Without a valid record:

```text
release gate: performance sign-off for v0.1.2 is incomplete:
  - PERF-GATE-1 (#170): no approval record at release-evidence:performance/0.1.2/PERF-GATE-1.json
```

## Cutting a release

1. Bump the version everywhere it is recorded and refresh the lock file:

   ```bash
   # Cargo.toml: [workspace.package] version = "0.1.2"
   # web/package.json: "version": "0.1.2"
   cargo update -w
   cd web && npm install --package-lock-only && cd ..
   ```

   In the same commit, add the release to `RELEASES` in
   `crates/server/src/datadir.rs` with the data-directory format and
   metadata schema it reads (`FORMAT`, `metadata::SCHEMA_VERSION`), and to
   the downgrade table in
   [configuration.md](configuration.md#downgrades). The unit test
   `releases_are_listed_oldest_first_and_never_shrink` fails for a
   non-prerelease version without a row, and for a row that does not match
   the build.

2. Check the tag before pushing it. This is the same check `preflight` runs:

   ```bash
   node scripts/release/release.mjs plan --tag v0.1.2
   ```

   ```text
   {
     "tag": "v0.1.2",
     "version": "0.1.2",
     "prerelease": false,
     "makeLatest": true,
     "imageTags": [
       "ghcr.io/rowan-smith/vyrtel:0.1.2",
       "ghcr.io/rowan-smith/vyrtel:0.1",
       "ghcr.io/rowan-smith/vyrtel:latest"
     ]
   }
   ```

   A mismatch fails before anything is built:

   ```text
   $ node scripts/release/release.mjs plan --tag v0.1.2     # workspace still at 0.1.1
   release gate: tag v0.1.2 is version 0.1.2 but Cargo.toml [workspace.package] = 0.1.1, Cargo.lock (server) = 0.1.1, web/package.json = 0.1.1; bump the workspace version (and run cargo update -w) before tagging
   ```

3. Merge the version bump to `main`. That commit is the candidate: run each gate's profiling on
   it and commit the PASS records to `release-evidence`
   ([Pre-tag performance sign-off](#pre-tag-performance-sign-off)).

4. Create the tag through the pre-tag check, on a clean checkout of the candidate, then push it:

   ```bash
   git switch --detach <candidate sha>
   node scripts/release/release.mjs pretag --tag v0.1.2 --create
   git push origin v0.1.2
   ```

   `pretag` fetches `release-evidence`. It refuses uncommitted changes, an existing tag, a
   version mismatch and any missing, failed or stale sign-off. Only then does it create an
   annotated tag whose message lists each gate's PASS. Do not create release tags with plain
   `git tag`: the workflow rejects a lightweight tag for a version that has gates.
   `--evidence <dir>` reads records from a local directory laid out like the branch instead.

5. Watch the **Release** workflow. When it is green, the GitHub release and image are live.

For a prerelease, use a version with a suffix in all three files and the tag, for example
`0.1.2-rc.1` / `v0.1.2-rc.1`.

## After a release

Add the new release's data-directory fixture, so every later build is
tested against data it wrote (REL-07). On Linux or macOS, with the
published binary for that platform:

```bash
VYRTEL_FIXTURE_BINARY=./vyrtel-0.1.2 VYRTEL_FIXTURE_OUT=tests/fixtures/datadir/v0.1.2 \
  cargo test -p server --test integration -- --ignored --exact upgrade::make_datadir_fixture
cargo test -p server --all-features --test integration upgrade
```

Commit `tests/fixtures/datadir/v0.1.2/` (see its
[README](../tests/fixtures/datadir/README.md)).

## Rehearsing a release

Run **Release** manually (Actions → Release → Run workflow), choose the branch and enter the tag
you intend to push, for example `v0.1.2-rc.1`. Every gate runs against that branch (preflight,
performance sign-off for the branch's head commit, full CI, all four binaries, artifact verification, the image checks on amd64 and arm64), and the log shows the
plan: channel, `make_latest` and image tags. Nothing is pushed to GHCR and no GitHub release is
created: a manual run cannot publish. The verified binaries are kept as the run's
`release-artifacts` artifact for inspection.

## Checking artifacts locally

```bash
# A local build: version output and embedded UI.
cd web && npm ci && npm run build && cd ..
VYRTEL_REQUIRE_WEB_UI=1 cargo build --release --locked -p server
node scripts/release/release.mjs verify-binary target/release/vyrtel
# target/release/vyrtel: version 0.1.1, embedded UI ok (/assets/index-….js)

# A running server or container.
docker build --build-arg VYRTEL_VERSION=0.1.1 -t vyrtel:rc . && docker run -d --name vyrtel-rc -p 8080:8080 vyrtel:rc
node scripts/release/release.mjs verify-server http://localhost:8080

# An image, as an operator runs it (see Container image checks).
node scripts/release/release.mjs verify-image vyrtel:rc

# Downloaded release assets: put the four binaries and SHA256SUMS in one directory.
node scripts/release/release.mjs verify-artifacts ./downloads
# or, without Node:
cd downloads && sha256sum -c SHA256SUMS
```

`--version <x.y.z>` overrides the expected version (default: the workspace version).
`verify-binary` starts the binary on a free `127.0.0.1` port, with an empty temporary data
directory and without any `VYRTEL_*` environment variables, and stops it afterwards.

## When a release fails

Nothing is published by a failed run, so there is nothing to roll back.

| Failure                                                                     | Fix                                                                                                                       |
|-----------------------------------------------------------------------------|---------------------------------------------------------------------------------------------------------------------------|
| `preflight`: version mismatch                                               | Delete the tag (`git push --delete origin v0.1.2 && git tag -d v0.1.2`), merge the version bump, tag again.               |
| `pretag`/`performance`: no approval record, or not PASS                     | Complete the gate's profiling and review, commit the record to `release-evidence`, run `pretag` again.                    |
| `pretag`/`performance`: `stale: approved candidate …`                       | The candidate changed after sign-off. Re-run the affected workloads on the new commit and record a new PASS.              |
| `pretag`/`performance`: `signed off different candidates`                   | Gates sharing a version approved different commits; re-run so all approve the same one.                                   |
| `performance`: not an annotated tag, or reviewed after the tag              | The tag bypassed `pretag`. Delete it and create it with `pretag --create` once the sign-off is recorded.                  |
| `verify`: built artifact differs from the approved hash                     | Confirm the rebuild is equivalent and add `equivalence` to the record, or re-profile the rebuilt artifact.                |
| `ci`: a required check                                                      | Fix it on `main` through a pull request. The fix is a new commit, so it needs a new performance sign-off before tagging.  |
| `binaries`: `VYRTEL_REQUIRE_WEB_UI is set but …/web/dist has no index.html` | The UI artifact from CI was not available; re-run the workflow.                                                           |
| `binaries`/`verify`/`image`: `web UI is not embedded` or a version mismatch | A build embedded the placeholder or an old version; fix the build and re-tag.                                             |
| `verify`: missing, unexpected or mismatched artifact                        | Re-run the failed jobs; if it repeats, a build or upload step is broken.                                                  |
| `image-smoke`: `runs as user …` or `label … expected`                       | The Dockerfile lost its `USER`, or the build its `VYRTEL_VERSION` build arg. Fix it on `main` and re-tag.                 |
| `image-smoke`: `data did not survive recreating the container`              | Acknowledged events were lost across a clean stop: a storage regression. Fix it and re-tag; never skip the step.          |
| `image-smoke`: `without the uid hint` or `started on a bind mount`          | The diagnostic in `crates/server/src/access.rs` changed, or the image runs as root. Fix it and re-tag.                    |
| `image`: `the pushed image has no linux/arm64 variant`                      | The push built fewer platforms than `image-smoke` tested. Fix `platforms:` and re-run the `image` job.                    |
| `release` fails after `image` pushed                                        | The image tags are already on GHCR. Re-run the `release` job; the binaries were verified and kept as `release-artifacts`. |

Re-running failed jobs on the same tag is safe. Re-tagging a different commit with a
version that was already published is not: publish a new patch version instead.

## Limitations

- The embedded-UI and version checks run each binary natively, so they need a runner per
  platform; there is no Linux ARM64 or macOS x86_64 check without one.
- The pushed multi-architecture image is rebuilt from the same commit, Dockerfile and build
  arguments as the two smoke-tested images, reusing their build caches, rather than being the
  smoke-tested images themselves. The registry check afterwards covers its user, label and
  platforms but does not start it.
- Pull requests smoke-test only `linux/amd64`; `linux/arm64` is checked by `image-smoke` in
  the release workflow, including a manual rehearsal.
- The bind-mount step needs a Linux host. The documented fix (`chown`) is exercised only where
  the runner may chown (root or passwordless `sudo`, as on GitHub-hosted runners); otherwise
  only the refusal is checked, and the log says so.
- Kubernetes, rootless or user-namespace-remapped Docker, and Windows containers are not tested.
- The image is pushed before the GitHub release is created, so a failure in the `release` job
  alone can leave a pushed image without a GitHub release (see above).
- Package-manager distribution (Homebrew, apt, winget, crates.io) is not part of the release.
- The performance check verifies the approval record, not the measurements: the profiling and
  its reports come from each gate's issue, and the reviewer is accountable for them. Anyone who
  can push to `release-evidence` can write a record, so protect that branch.
- `pretag` cannot stop someone running plain `git tag`. The workflow still refuses a
  lightweight or early tag; a tag ruleset restricting who may create `v*` tags adds a
  server-side guard.
