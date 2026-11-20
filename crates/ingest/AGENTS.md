# Ingestion Rules

`ingest` converts native JSON/NDJSON and OTLP inputs into the shared telemetry model.

- Keep parsing and field mapping here; WAL writes belong to `storage`, and HTTP admission/auth belong to `server`.
- Validate malformed and oversized inputs without panics or unbounded allocation.
- Preserve useful fields, timestamps, signal semantics and documented input compatibility.
- Make rejection and partial-accept behaviour explicit; never report unaccepted data as accepted.
- Test malformed payloads, decompression edge cases, boundary sizes and representative OTLP/native fixtures.

When changing request limits or backpressure, coordinate with `server` and its existing memory budget.
