# Data-directory fixtures

Data directories written by released Vyrtel versions, for the upgrade tests
in `tests/integration/upgrade.rs` (REL-07). Every later build must start
them and answer the
[verification queries](../../../docs/configuration.md#verification-queries)
exactly as the release did, except `v0.1.0/`: 0.1.0 was released as
Observer, whose files Vyrtel does not read, so every build must refuse it
with nothing changed
([Data from Observer 0.1.0](../../../docs/configuration.md#data-from-observer-010)).

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
  cargo test -p server --test integration -- --ignored --exact upgrade::make_datadir_fixture
```

`make_datadir_fixture` starts that binary with authentication, a 1 s
segment age and retention off, creates the keys and configuration through
the API, ingests the shared fixtures in `tests/fixtures/` (waiting until
logs and traces are sealed before the rest), records the verification
answers, stops it with SIGTERM and writes `fixture.json`. Never edit a
fixture by hand.

| Fixture   | Written by                         | Built from      |
|-----------|------------------------------------|-----------------|
| `v0.1.0/` | Observer 0.1.0 (debug build), `OBSERVER_*` settings | tag `v0.1.0` |
| `v0.1.1/` | the `v0.1.1` release (debug build) | tag `v0.1.1`    |

`v0.1.0` and `v0.1.1` were built from their tags with `cargo build -p
server` in the `rust:1-bookworm` image (Linux x86_64) and written there.
