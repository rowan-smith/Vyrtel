# Test fixtures

Shared by the Rust integration tests (`tests/integration/`), the Playwright
E2E suite (`web/tests/e2e/`) and the documentation examples.

* `logs.ndjson` — native NDJSON log events (no timestamps: the server stamps
  them on arrival, so they always fall inside "last hour").
* `otlp-*.json` — OTLP/JSON payloads. Placeholders like `{{now-900ms}}` are
  replaced with Unix-nanosecond timestamps relative to the current time
  before sending (see `fixture()` in `tests/integration/common/mod.rs` and
  `loadFixture()` in `web/tests/e2e/fixtures.ts`). Units: `ms`, `s`, `m`.

All fixtures share trace id `4bf92f3577b34da6a3ce929d0e0e4736`, so logs and
spans correlate.
