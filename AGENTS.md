# Vyrtel Engineering Rules

Vyrtel is a small, fast, self-hosted observability platform: one process, one data directory, no required external services.

## Architecture

- Respect the crate roles in `docs/architecture.md`: `telemetry` owns the event model; `ingest` parses inputs; `storage` owns durable telemetry; `query` searches it; `metadata` owns SQLite product state; `server` coordinates HTTP, auth and the UI.
- Keep telemetry out of SQLite. Treat crate boundaries as code boundaries, not reasons to add services.
- Prefer straightforward code over generic frameworks and speculative abstractions.
- Justify new dependencies. Do not add infrastructure or network dependencies without an explicit requirement.

## Non-negotiables

- Bound memory, queues, concurrency and work under overload; return clear backpressure instead of growing indefinitely.
- Preserve accepted data, crash recovery, storage compatibility and API contracts.
- Never weaken authentication, access scopes or secret handling as a side effect of another task.
- Keep the app usable on modest hardware; measure performance claims rather than guessing.

## Working style

- Make focused changes; follow nearby conventions and existing formatters.
- Add or update tests for behaviour and failure cases. Document user-visible changes.
- Read `docs/development.md`, `docs/testing.md` and relevant design docs rather than repeating them here.
- For feature work, follow `.agents/skills/feature-implementation/SKILL.md` when supported.
- When working a GitHub issue, follow `.agents/skills/ticket-workflow/SKILL.md` for its Project status and PR description.
- Branch, name and merge changes as described in `.agents/skills/branching-strategy/SKILL.md`.
- Before finishing, run relevant checks and state what ran, what failed and what was not verified.
