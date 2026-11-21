# Server and Security Rules

`server` owns HTTP transport, routing, authentication, admission and application wiring.

## Authentication contract

- The UI and API always require a login or an admin key. A fresh install generates and prints an admin password once; it can be changed.
- Ingestion is a separate gate, decided by the **ingestion mode** (`automatic`, `required`, `open`) and the **protection latch**, both stored in the metadata database.
- In `automatic` (the default), ingestion is open until the first ingest-scoped API key is created; that sets the latch, and ingestion then requires a valid key.
- **The latch is permanent.** Revoking keys, the last ingest key included, and restarting leave it set: removing credentials must never open writes. Ingestion becomes open only when an admin deliberately chooses the `open` mode.
- Always enforce key scopes; a key must never gain unrelated API access. `open` opens ingestion only.
- Authentication state changes must survive restart and must not expose a transient unauthorised path. Unreadable mode or latch state fails closed.

This contract is documented in `docs/configuration.md` ("Ingestion authentication"). Do not change it as a side effect of other work; a change needs its own reviewed, tested change and documentation update.

## Transport

- Enforce body, connection, queue and concurrency limits at admission boundaries.
- Prefer explicit 4xx/429/503 responses and observability over hangs or silent failures.
- Keep health/readiness truthful and shutdown bounded and safe.
- Never leak credentials, tokens or sensitive request content into logs.

Read `docs/guide/security.md` and `docs/architecture.md` when modifying these paths.
