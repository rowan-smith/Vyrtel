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
| `ci`        | The whole [CI workflow](../.github/workflows/ci.yml) against the tagged commit: fmt, clippy, Rust tests, the restore drill, frontend lint/types/tests, Playwright E2E, Docker smoke test, dependency checks, .NET build and the release gate tests.                                                                                                | Any CI job fails.                                                                         |
| `binaries`  | Builds each target with the UI that passed CI and `VYRTEL_REQUIRE_WEB_UI=1`, so a missing UI is a build error. On its own runner, each binary must print `vyrtel <version>` and, started on an empty data directory, report that version from `/api/v1/system/info` and serve the built UI and its `/assets/*.js` entry script. | Build fails; wrong version; placeholder UI; missing UI assets; the server does not start. |
| `verify`    | Combines the per-target checksums into `SHA256SUMS`, then checks the artifact set is exactly the four binaries, none is empty, and every checksum matches. Re-runs the binary check on the Linux x86_64 binary as downloaded. Checks the built artifacts have the hashes the performance gates profiled, or that the record documents reproducible equivalence. | A binary is missing, empty, unexpected or does not match its checksum; a profiled artifact's hash differs with no documented equivalence. |
| `image`     | Builds a `linux/amd64` candidate and checks `--version`, the version reported by the API, the embedded UI and `vyrtel healthcheck`. Only then pushes the multi-architecture image with the preflight image tags.                                                                                                                | Any image check fails.                                                                    |
| `release`   | Re-checks `SHA256SUMS` and creates the GitHub release from the verified artifacts, as a prerelease (not Latest) for prerelease tags.                                                                                                                                                                                            | Checksums no longer match.                                                                |

The checks are in [`scripts/release/release.mjs`](../scripts/release/release.mjs) (Node 22, no
dependencies) so they can be run locally exactly as the workflow runs them. The performance
checks are in [`scripts/release/performance.mjs`](../scripts/release/performance.mjs).

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
performance sign-off for the branch's head commit, full CI, all four binaries, artifact verification, the image smoke test), and the log shows the
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
docker build -t vyrtel:rc . && docker run -d --name vyrtel-rc -p 8080:8080 vyrtel:rc
node scripts/release/release.mjs verify-server http://localhost:8080

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
| `release` fails after `image` pushed                                        | The image tags are already on GHCR. Re-run the `release` job; the binaries were verified and kept as `release-artifacts`. |

Re-running failed jobs on the same tag is safe. Re-tagging a different commit with a
version that was already published is not: publish a new patch version instead.

## Limitations

- The embedded-UI and version checks run each binary natively, so they need a runner per
  platform; there is no Linux ARM64 or macOS x86_64 check without one.
- Only the `linux/amd64` image is smoke-tested before the push; the `linux/arm64` image is built
  from the same Dockerfile and sources (with `VYRTEL_REQUIRE_WEB_UI=1`) but not started.
- The image is pushed before the GitHub release is created, so a failure in the `release` job
  alone can leave a pushed image without a GitHub release (see above).
- Package-manager distribution (Homebrew, apt, winget, crates.io) is not part of the release.
- The performance check verifies the approval record, not the measurements: the profiling and
  its reports come from each gate's issue, and the reviewer is accountable for them. Anyone who
  can push to `release-evidence` can write a record, so protect that branch.
- `pretag` cannot stop someone running plain `git tag`. The workflow still refuses a
  lightweight or early tag; a tag ruleset restricting who may create `v*` tags adds a
  server-side guard.
