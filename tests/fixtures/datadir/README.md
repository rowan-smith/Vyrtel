# Data-directory fixtures

Data directories written by released Vyrtel versions, for the layout tests
in `tests/integration/layouts.rs` (COMPAT-GATE-0.1.2). Before 1.0 a release
reads only data directories of its own layout
([Upgrading between pre-1.0 releases](../../../docs/configuration.md#upgrading-between-pre-10-releases)):

* a fixture of the build's layout (its `data/vyrtel.json` has the build's
  `"layout"`) must start and answer the
  [verification queries](../../../docs/configuration.md#verification-queries)
  exactly as its release did;
* every other fixture must be refused by `check-data-dir` and by a start,
  with every file's SHA-256 unchanged.

`v0.1.0/` (released as Observer) and `v0.1.1/` predate layout identities,
so every build from 0.1.2 on refuses them. They are kept as real examples
of data a release must refuse: files that say version 1 like the current
ones but are laid out differently.

Each `v<version>/` holds:

* `data/` — the data directory, as the release left it after a clean stop
  (SIGTERM, exit status 0). It holds all three signals, some sealed into
  segments and some still in the WAL, a dashboard with a panel, a disabled
  alert, a saved query, an API key and a revoked one, and the admin user.
* `fixture.json` — `writtenBy` (`vyrtel --version` of the release), the
  API key and the revoked key, `answers` (the verification answers the
  release gave just before it was stopped) and `layout`: every directory,
  and every file with its size and SHA-256.

Git keeps no empty directories, so the tests recreate them from `layout`
and then check every file's size and SHA-256. `.gitattributes` marks
`data/` binary so no checkout converts line endings.

The admin password is `fixture-password`, and the API keys in
`fixture.json` exist only in these directories: they are test values. The
events carry the timestamps of when the fixture was made, so start them
with retention off (`VYRTEL_RETENTION_*=forever`) or they are deleted as
expired.

## Adding a fixture

After each release, on Linux or macOS, with that release's binary:

```bash
VYRTEL_FIXTURE_BINARY=/path/to/vyrtel-0.1.2 VYRTEL_FIXTURE_OUT=tests/fixtures/datadir/v0.1.2 \
  cargo test -p server --test integration -- --ignored --exact layouts::make_datadir_fixture
```

`make_datadir_fixture` starts that binary with authentication, a 1 s
segment age and retention off, creates the keys and configuration through
the API, ingests the shared fixtures in `tests/fixtures/` (waiting until
logs and traces are sealed before the rest), records the verification
answers, stops it with SIGTERM and writes `fixture.json`. Never edit a
fixture by hand. A fixture is never deleted because a later layout no
longer reads it: it then tests the refusal.

| Fixture   | Written by                         | Built from      | Layout identity |
|-----------|------------------------------------|-----------------|-----------------|
| `v0.1.0/` | Observer 0.1.0 (debug build), `OBSERVER_*` settings | tag `v0.1.0` | none |
| `v0.1.1/` | the `v0.1.1` release (debug build) | tag `v0.1.1`    | none            |

`v0.1.0` and `v0.1.1` were built from their tags with `cargo build -p
server` in the `rust:1-bookworm` image (Linux x86_64) and written there.
