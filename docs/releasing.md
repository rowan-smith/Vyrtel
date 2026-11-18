# Releasing

Releases are published by [`.github/workflows/release.yml`](../.github/workflows/release.yml)
when a `v*` tag is pushed. The workflow publishes only after the release has been validated:
if any check fails, no binaries, image or GitHub release are published.

Some validation happens before the tag exists, as manual steps for the maintainer: the
compatibility cleanup ([Pre-tag compatibility review](#pre-tag-compatibility-review)) and the
milestone's performance profiling ([Performance profiling](#performance-profiling)). No tool
checks either of them.

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
| `ci`        | The whole [CI workflow](../.github/workflows/ci.yml) against the tagged commit: fmt, clippy, Rust tests, the restore drill, frontend lint/types/tests, Playwright E2E, the Docker [image checks](#container-image-checks) on `linux/amd64`, dependency checks, .NET build and the release gate tests.                                                                                                | Any CI job fails.                                                                         |
| `binaries`  | Builds each target with the UI that passed CI and `VYRTEL_REQUIRE_WEB_UI=1`, so a missing UI is a build error. Linux targets build in the `rust:1-bookworm` container on pinned runners, with C dependencies set to the x86-64 baseline. On its own runner, each binary must print `vyrtel <version>` and, started on an empty data directory, report that version from `/api/v1/system/info` and serve the built UI and its `/assets/*.js` entry script. Linux binaries must need no glibc newer than 2.34, and the x86_64 binary must pass the whole check under `qemu-x86_64 -cpu qemu64`, a CPU without SSE4 or AVX. | Build fails; wrong version; placeholder UI; missing UI assets; the server does not start; a glibc symbol newer than 2.34; the x86_64 binary does not start on the baseline CPU. |
| `verify`    | Combines the per-target checksums into `SHA256SUMS`, then checks the artifact set is exactly the four binaries, none is empty, and every checksum matches. Re-runs the binary check, including the glibc and baseline-CPU checks, on the Linux x86_64 binary as downloaded. | A binary is missing, empty, unexpected or does not match its checksum. |
| `image-smoke` | On native `linux/amd64` and `linux/arm64` runners, builds the image with the release version and runs every [image check](#container-image-checks): non-root user and version label, `--version`, readiness and the embedded UI on a named volume, events surviving a recreated container, and the uid hint on an unwritable bind mount. Only then pushes that platform's image **by digest, untagged**, and checks the pushed digest has exactly the tested image's layers. | Any image check fails on either architecture, or the pushed digest is not the tested image. |
| `image`     | Only after both platforms pass, tags their two tested digests with the preflight image tags as one multi-platform image (`docker buildx imagetools create`; nothing is rebuilt), then inspects it in the registry: both platforms present, each running as `nonroot` with the release version label and the tested layers.      | A tested digest is missing, or the published image lacks a platform, the user, the label or the tested layers. |
| `release`   | Re-checks `SHA256SUMS` and creates the GitHub release from the verified artifacts, as a prerelease (not Latest) for prerelease tags.                                                                                                                                                                                            | Checksums no longer match.                                                                |

The checks are in [`scripts/release/release.mjs`](../scripts/release/release.mjs) (Node 22, no
dependencies) so they can be run locally exactly as the workflow runs them. The image checks
are in [`scripts/release/image.mjs`](../scripts/release/image.mjs).

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
| bind mount | A host directory owned by the runner's user with mode `555` is mounted at `/data`, detached: the server must exit non-zero with the [uid hint](configuration.md#docker-and-bind-mounts). If it prints `Ready.`, or is still running after the timeout, the step fails rather than waiting, and the container is removed. With root or passwordless `sudo`, the directory is then given to 65532 as documented and the server must start, answer and stop cleanly. |

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
step. That is intended: the label is what operators and registries read.

**Publishing exactly what was tested.** `--record <dir>` writes `<dir>/linux-<arch>.json` once
every check has passed: the platform and the tested image's layer diff IDs (the digests of its
uncompressed layers, which identify its files whatever the compression, tag or manifest list).
`verify-pushed-image <ref> --tested <dir>` inspects `<ref>` in the registry
(`docker buildx imagetools inspect`, no pull), runs the config step on each recorded platform
and requires its layers to equal the recorded ones. The release runs it twice: on each
platform's untagged digest right after the push, and on the tagged release image. Without
`--tested` it checks the config of every published platform only.
The registry view of an image config is the OCI one, which has no field for Docker's
HEALTHCHECK, so the registry check skips only the healthcheck; `verify-image` checks it on the
tested image.

## Pre-tag compatibility review

Before 1.0, Vyrtel keeps no compatibility code merely because an earlier 0.x release had it
([policy](../CONTRIBUTING.md#compatibility-policy-before-10)). Before tagging a release, the
maintainer checks that the candidate followed it. This is a manual step: neither `pretag` nor
the release workflow checks it, so it is the maintainer's responsibility to do it.

For each release version, open a `COMPAT-GATE-<version>` issue from the **Release compatibility
gate** issue template ([`compat-gate.yml`](../.github/ISSUE_TEMPLATE/compat-gate.yml)) and work
through it on the candidate:

- **Inventory and remove** obsolete compatibility paths, and change the contracts (formats,
  routes, settings) with tests and docs.
  [docs/compatibility/0.1.2.md](compatibility/0.1.2.md) is the worked example
  ([#229](https://github.com/rowan-smith/Vyrtel/issues/229)).
- **Formats and layout**: if any persisted format changed since the last release, every format
  version is reset to 1 and `telemetry::LAYOUT` names this release
  ([storage format](storage-format.md#layout-changes)); otherwise it keeps the last layout.
  The release table in [configuration.md](configuration.md#upgrading-between-pre-10-releases)
  is up to date.
- **Exceptions**: a temporary compatibility exception needs Rowan's approval, an owner, a
  removal release and a linked issue.

Record the outcome on the issue. Green CI alone does not show the cleanup was done, and "nothing
to remove" needs the audit that shows it. All prereleases of a version share its issue.

### Release order

1. **Clean up**, as above, early in the milestone.
2. **Verify** correctness and recovery on the integrated candidate (CI, crash and fault tests,
   the restore drill), and confirm the compatibility review on its issue.
3. **Profile** the same commit for the milestone's performance gate, where one is run
   ([Performance profiling](#performance-profiling)). Do the compatibility review first, so
   that the profiled commit is the cleaned-up one.
4. **Tag** through `pretag --create`.

Any artifact-affecting change after step 2 makes a new commit: review the change, and re-measure
it if it was profiled.

## Performance profiling

Each delivery milestone has a `PERF-GATE-*` issue (for example
[#170](https://github.com/rowan-smith/Vyrtel/issues/170) for 0.1.2) describing the profiling of
its final candidate: workloads, metrics, budgets and how to compare against the previous release.
The tools are the [reproducible baselines](../dotnet/Vyrtel.Baseline/README.md), the
[transport soak](testing.md#transport-soak-on-linux) with
[`scripts/bench/transport-budgets.md`](../scripts/bench/transport-budgets.md), and the
[benchmark method](benchmark-method.md).

This is a manual step. The release tooling records and checks no performance sign-off: neither
`pretag` nor the release workflow looks for one, and nothing stops a tag without it. Published
measurements go on the website; the per-version reports in this repository are in
[benchmarks](benchmarks.md).

0.1.2 was released without the final-candidate profiling in
[#170](https://github.com/rowan-smith/Vyrtel/issues/170). The measurements made before release
are in [0.1.2-rc.5](benchmarks/0.1.2-rc.5.md).

## Cutting a release

1. Bump the version everywhere it is recorded and refresh the lock file:

   ```bash
   # Cargo.toml: [workspace.package] version = "0.1.2"
   # web/package.json: "version": "0.1.2"
   cargo update -w
   cd web && npm install --package-lock-only && cd ..
   ```

   Check `telemetry::LAYOUT` (`crates/telemetry/src/layout.rs`): if any persisted format changed
   since the last release, it must name this release and every format version must be 1
   ([storage format](storage-format.md#layout-changes)); if none changed, it keeps the last
   layout. Update the release table in
   [configuration.md](configuration.md#upgrading-between-pre-10-releases) either way, as part of
   the [compatibility review](#pre-tag-compatibility-review).

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
     ],
     "imageRef": "ghcr.io/rowan-smith/vyrtel:0.1.2",
     "imageRepo": "ghcr.io/rowan-smith/vyrtel"
   }
   ```

   A mismatch fails before anything is built:

   ```text
   $ node scripts/release/release.mjs plan --tag v0.1.2     # workspace still at 0.1.1
   release gate: tag v0.1.2 is version 0.1.2 but Cargo.toml [workspace.package] = 0.1.1, Cargo.lock (server) = 0.1.1, web/package.json = 0.1.1; bump the workspace version (and run cargo update -w) before tagging
   ```

3. Merge the version bump to `main`. That commit is the candidate. Confirm its
   [compatibility review](#pre-tag-compatibility-review), and run its
   [performance profiling](#performance-profiling) if one is planned
   ([Release order](#release-order)).

4. Create the tag through the pre-tag check, on a clean checkout of the candidate, then push it:

   ```bash
   git switch --detach <candidate sha>
   node scripts/release/release.mjs pretag --tag v0.1.2 --create
   git push origin v0.1.2
   ```

   `pretag` refuses uncommitted changes, an existing tag and a version mismatch. Only then does
   it create an annotated tag (`Vyrtel <version>`). Use it rather than plain `git tag`, so the
   tag is checked first and annotated.

5. Watch the **Release** workflow. When it is green, the GitHub release and image are live.

For a prerelease, use a version with a suffix in all three files and the tag, for example
`0.1.2-rc.1` / `v0.1.2-rc.1`, and tag it with `pretag --create` the same way.

## After a release

The website redeploys itself once a stable release is published: the release's docs appear at
`/docs/<version>/` and become the default docs. Nothing needs to be copied.

Add the new release's data-directory fixture, so every later build is
tested against data it wrote: a build of the same layout must read it, one
of another layout must refuse it unchanged. On Linux or macOS, with the
published binary for that platform:

```bash
VYRTEL_FIXTURE_BINARY=./vyrtel-0.1.2 VYRTEL_FIXTURE_OUT=tests/fixtures/datadir/v0.1.2 \
  cargo test -p server --test integration -- --ignored --exact layouts::make_datadir_fixture
cargo test -p server --all-features --test integration layouts
```

Commit `tests/fixtures/datadir/v0.1.2/` (see its
[README](../tests/fixtures/datadir/README.md)).

## Rehearsing a release

Run **Release** manually (Actions → Release → Run workflow), choose the branch and enter the tag
you intend to push, for example `v0.1.2-rc.1`. Every gate runs against that branch (preflight,
full CI, all four binaries, artifact verification, the image checks on amd64 and arm64), and the log shows the
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
`--cpu <model>` runs it through `qemu-x86_64 -cpu <model>` (package `qemu-user`), so
`--cpu qemu64` proves an x86_64 binary starts without SSE4, AVX or AVX2. `--max-glibc 2.34`
fails when `readelf --dyn-syms` shows any symbol needing a newer glibc, and names it.

```bash
node scripts/release/release.mjs verify-binary vyrtel-linux-x86_64 --cpu qemu64 --max-glibc 2.34
# vyrtel-linux-x86_64: needs glibc 2.34, within 2.34
# vyrtel-linux-x86_64: version 0.1.2, embedded UI ok (/assets/index-….js), starts on CPU model qemu64
```

## When a release fails

Nothing is published by a failed run, so there is nothing to roll back.

| Failure                                                                     | Fix                                                                                                                       |
|-----------------------------------------------------------------------------|---------------------------------------------------------------------------------------------------------------------------|
| `preflight`: version mismatch                                               | Delete the tag (`git push --delete origin v0.1.2 && git tag -d v0.1.2`), merge the version bump, tag again.               |
| `pretag`: uncommitted changes, existing tag or version mismatch             | Commit or stash local changes; never re-tag a published version; bump the version first.                                  |
| `ci`: a required check                                                      | Fix it on `main` through a pull request. The fix is a new commit; re-run the release order's checks on it before tagging. |
| `binaries`: `VYRTEL_REQUIRE_WEB_UI is set but …/web/dist has no index.html` | The UI artifact from CI was not available; re-run the workflow.                                                           |
| `binaries`/`verify`/`image`: `web UI is not embedded` or a version mismatch | A build embedded the placeholder or an old version; fix the build and re-tag.                                             |
| `binaries`/`verify`: `needs glibc 2.x, newer than the supported 2.34`       | The build ran against newer glibc headers (often a runner image or container change). Build in `rust:1-bookworm` as the workflow does; do not raise the limit without deciding to drop older distributions. |
| `binaries`/`verify`: `exited SIGILL` under `--cpu qemu64`                   | Code was compiled for a newer CPU (AVX/AVX2), usually through C dependencies picking up the build host's flags. Check `CFLAGS_*`/`RUSTFLAGS` and the runner image; do not ship it. |
| `verify`: missing, unexpected or mismatched artifact                        | Re-run the failed jobs; if it repeats, a build or upload step is broken.                                                  |
| `image-smoke`: `runs as user …` or `label … expected`                       | The Dockerfile lost its `USER`, or the build its `VYRTEL_VERSION` build arg. Fix it on `main` and re-tag.                 |
| `image-smoke`: `data did not survive recreating the container`              | Acknowledged events were lost across a clean stop: a storage regression. Fix it and re-tag; never skip the step.          |
| `image-smoke`: `without the uid hint` or `started on a bind mount`          | The diagnostic in `crates/server/src/access.rs` changed, or the image runs as root. Fix it and re-tag.                    |
| `image-smoke`: `the pushed linux/… image is not the one that was smoke-tested` | The push-by-digest build did not reuse the tested build (a cache miss with a non-reproducible step). Re-run `image-smoke`; nothing was tagged. |
| `image-smoke`: `the server … became ready` or `still running … after starting on a bind mount` | The image starts on a directory its user cannot write: it runs as root, or the bind-mount check broke. Fix it and re-tag. |
| `image`: `expected 2 tested digests` or `has no linux/arm64 variant`        | A platform's tested digest was not uploaded. Re-run the failed jobs; nothing was tagged.                                  |
| `release` fails after `image` pushed                                        | The image tags are already on GHCR. Re-run the `release` job; the binaries were verified and kept as `release-artifacts`. |
| `image-smoke` fails after one platform pushed                               | Its untagged digest stays in GHCR but no tag points at it. It can be deleted from the package's versions; a re-run pushes again. |

Re-running failed jobs on the same tag is safe. Re-tagging a different commit with a
version that was already published is not: publish a new patch version instead.

## Limitations

- The embedded-UI and version checks run each binary natively, so they need a runner per
  platform; there is no Linux ARM64 or macOS x86_64 check without one.
- Each platform's image is pushed by a second build in the same job, reusing the tested
  build's cache rather than exporting the loaded image. The layer check proves the pushed
  image has exactly the tested files; if a cache miss produced different layers, the release
  fails instead of publishing. The rest of the image config is checked (user, version label,
  platform, volume, environment, healthcheck) but not compared byte for byte with the tested one.
- Pull requests smoke-test only `linux/amd64`; `linux/arm64` is checked by `image-smoke` in
  the release workflow, including a manual rehearsal.
- The bind-mount step needs a Linux host. The documented fix (`chown`) is exercised only where
  the runner may chown (root or passwordless `sudo`, as on GitHub-hosted runners); otherwise
  only the refusal is checked, and the log says so.
- Kubernetes, rootless or user-namespace-remapped Docker, and Windows containers are not tested.
- The image is pushed before the GitHub release is created, so a failure in the `release` job
  alone can leave a pushed image without a GitHub release (see above).
- Package-manager distribution (Homebrew, apt, winget, crates.io) is not part of the release.
- The compatibility review and performance profiling are not checked by any tool; the
  maintainer is accountable for doing them before tagging.
- `pretag` cannot stop someone running plain `git tag`, and the workflow publishes a lightweight
  tag too. A tag ruleset restricting who may create `v*` tags adds a server-side guard.
