---
name: branching-strategy
description: Choose, name and land a Vyrtel branch using the trunk-based workflow — short-lived branches from main, squash-merged pull requests, and release tags cut from main.
---

# Branching strategy

Vyrtel is trunk-based. `main` is the only long-lived branch; it is always releasable and every
change reaches it through a pull request with CI passing.

1. Start from an up-to-date `main` without tracking it:
   `git fetch origin && git switch --no-track -c <type>/<topic> origin/main`. A branch that
   tracks `origin/main` pushes to `main` from a plain `git push` or an IDE push.
2. Name the branch `<type>/<short-kebab-topic>` (see below). One focused change per branch.
3. Keep the branch short-lived. Rebase on `origin/main` rather than merging `main` into it, and
   only force-push (`--force-with-lease`) branches you own.
4. Push the branch to its own name the first time (`git push -u origin <type>/<topic>`), then
   open a pull request into `main`. CI (`.github/workflows/ci.yml`) runs on every pull request;
   do not merge with required checks failing or skipped.
5. Land it with **squash merge**, so `main` gets one commit titled `<Imperative summary> (#N)`.
   Delete the branch after merging.

## Branch names

| Prefix                  | Use for                                                  | Example                        |
| ----------------------- | -------------------------------------------------------- | ------------------------------ |
| `fix/`                  | Bug fixes and security fixes                             | `fix/malloc-arena-cap`         |
| `<area>/`               | Feature or behaviour work scoped to one area             | `storage/segment-handle-cache` |
| `docs/`                 | Documentation and website changes                        | `docs/benchmark-pages`         |
| `ci/`                   | Workflow and CI configuration                            | `ci/drop-withdrawn-advisory`   |
| `chore/`                | Tooling, agent kit, housekeeping                         | `chore/agent-kit`              |
| `bench/`, `benchmarks/` | Benchmark investigations and recorded results            | `benchmarks/0.1.2`             |
| `release/`              | A version-bump PR for one release (not a release branch) | `release/0.1.2-rc.4`           |

Area prefixes follow the crate or subsystem (`storage/`, `query/`, `ingest/`, `transport/`,
`auth/`, `web/`). `dependabot/…` branches are created by Dependabot; leave their names alone.

## Releases and fixes

- There are no long-lived release, develop or hotfix branches. A `release/X.Y.Z[-rc.N]` branch
  only carries the version bump; once merged, that `main` commit is the candidate
  (`docs/releasing.md`).
- Tags are cut from `main` with `node scripts/release/release.mjs pretag --tag vX.Y.Z --create`,
  never `git tag`, and only with explicit maintainer authorisation (see the `release-check` skill).
- A broken release is fixed forward on `main` through a normal pull request, then re-tagged as a
  new version. Do not patch or move an existing tag.
- Rehearse a release from a branch with the manual **Release** workflow; it publishes nothing.

## Do not

- Commit or push directly to `main`, or rewrite its history.
- Touch `cla-signatures`; the CLA bot owns it.
- Create evidence, staging or long-lived integration branches.
- Bundle unrelated changes on one branch to save a pull request.
