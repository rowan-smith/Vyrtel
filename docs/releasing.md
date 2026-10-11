# Releasing

Releases are published by [`.github/workflows/release.yml`](../.github/workflows/release.yml)
when a `v*` tag is pushed. The workflow publishes only after the release has been validated:
if any check fails, no binaries, image or GitHub release are published.

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
| `ci`        | The whole [CI workflow](../.github/workflows/ci.yml) against the tagged commit: fmt, clippy, Rust tests, frontend lint/types/tests, Playwright E2E, Docker smoke test, dependency checks, .NET build and the release gate tests.                                                                                                | Any CI job fails.                                                                         |
| `binaries`  | Builds each target with the UI that passed CI and `VYRTEL_REQUIRE_WEB_UI=1`, so a missing UI is a build error. On its own runner, each binary must print `vyrtel <version>` and, started on an empty data directory, report that version from `/api/v1/system/info` and serve the built UI and its `/assets/*.js` entry script. | Build fails; wrong version; placeholder UI; missing UI assets; the server does not start. |
| `verify`    | Combines the per-target checksums into `SHA256SUMS`, then checks the artifact set is exactly the four binaries, none is empty, and every checksum matches. Re-runs the binary check on the Linux x86_64 binary as downloaded.                                                                                                   | A binary is missing, empty, unexpected or does not match its checksum.                    |
| `image`     | Builds a `linux/amd64` candidate and checks `--version`, the version reported by the API, the embedded UI and `vyrtel healthcheck`. Only then pushes the multi-architecture image with the preflight image tags.                                                                                                                | Any image check fails.                                                                    |
| `release`   | Re-checks `SHA256SUMS` and creates the GitHub release from the verified artifacts, as a prerelease (not Latest) for prerelease tags.                                                                                                                                                                                            | Checksums no longer match.                                                                |

The checks are in [`scripts/release/release.mjs`](../scripts/release/release.mjs) (Node 22, no
dependencies) so they can be run locally exactly as the workflow runs them.

## Cutting a release

1. Bump the version everywhere it is recorded and refresh the lock file:

   ```bash
   # Cargo.toml: [workspace.package] version = "0.1.2"
   # web/package.json: "version": "0.1.2"
   cargo update -w
   cd web && npm install --package-lock-only && cd ..
   ```

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

3. Merge the version bump to `main`, then tag that commit and push the tag:

   ```bash
   git tag v0.1.2 && git push origin v0.1.2
   ```

4. Watch the **Release** workflow. When it is green, the GitHub release and image are live.

For a prerelease, use a version with a suffix in all three files and the tag, for example
`0.1.2-rc.1` / `v0.1.2-rc.1`.

## Rehearsing a release

Run **Release** manually (Actions → Release → Run workflow), choose the branch and enter the tag
you intend to push, for example `v0.1.2-rc.1`. Every gate runs against that branch (preflight,
full CI, all four binaries, artifact verification, the image smoke test), and the log shows the
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
| `ci`: a required check                                                      | Fix it on `main` through a pull request, then move the tag to the fixed commit (delete and re-push it).                   |
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
