# Roadmap

Vyrtel's planned work lives in [GitHub milestones](https://github.com/rowan-smith/Vyrtel/milestones) and the [project board](https://github.com/users/rowan-smith/projects/2). This page summarises that backlog as of **10 October 2026**. Issues hold the acceptance criteria, dependencies, priority, estimates, tests and documentation requirements.

**Current:** 0.1.2 — Reliability and release confidence. **Next:** 0.1.3 — Complete everyday workflows. Later milestones are planned work, not shipped features or committed release dates. Milestones sharing a version are separate workstreams for that release.

## Available today: v0.1.1

The released baseline is one binary and one data directory: native JSON/NDJSON and CLEF-compatible ingestion; OTLP/HTTP logs, traces and metrics; WAL and compressed segments; query filters and diagnostics; SSE live tail; trace waterfalls; basic metrics, dashboards and webhook alerts; per-signal retention; and Docker images. See the [overview](../README.md) and [release notes](https://github.com/rowan-smith/Vyrtel/releases) for current behaviour and limitations.

The backlog includes verification and hardening of existing behaviour as well as new capabilities. A planned story does not mean every part of its feature is absent today. Closing an issue does not by itself publish a release.

## Planned milestone sequence

| Release | Workstream | Position |
| --- | --- | --- |
| 0.1.2 | [Reliability and release confidence](https://github.com/rowan-smith/Vyrtel/milestone/1) | Current |
| 0.1.3 | [Complete everyday workflows](https://github.com/rowan-smith/Vyrtel/milestone/2) | Next |
| 0.2.0 | [Faster log investigations](https://github.com/rowan-smith/Vyrtel/milestone/3) | Planned |
| 0.2.0 | [Metrics correctness and interoperability](https://github.com/rowan-smith/Vyrtel/milestone/4) | Planned |
| 0.2.1 | [Retention and alert operations](https://github.com/rowan-smith/Vyrtel/milestone/5) | Planned |
| 0.2.1 | [Privacy and user audit trails](https://github.com/rowan-smith/Vyrtel/milestone/10) | Planned |
| 0.2.2 | [Dashboard investigation workflows](https://github.com/rowan-smith/Vyrtel/milestone/6) | Planned |
| 0.2.3 | [Security investigation workflows](https://github.com/rowan-smith/Vyrtel/milestone/11) | Planned |
| 0.3.0 | [Ingest and Core node foundation](https://github.com/rowan-smith/Vyrtel/milestone/8) | Planned |
| 0.3.1 | [Multi-node operational hardening](https://github.com/rowan-smith/Vyrtel/milestone/9) | Planned |
| Future | [Discovery and architecture decisions](https://github.com/rowan-smith/Vyrtel/milestone/7) | Discovery; no delivery commitment |

## 0.1.2 — Reliability and release confidence

Harden the single-node foundation before expanding everyday workflows.

* Crash recovery and acknowledgement verification, bounded ingest concurrency, decoded-payload limits, predictable storage failures and graceful shutdown.
* Complete stopped-server backup/restore, safe upgrades, recovery health, reproducible performance baselines and query execution budgets/cancellation.
* Validation-gated releases, persistent multi-architecture Docker smoke tests and trusted signed Windows executables. Signing needs a maintainer-provisioned signing identity/service.

[Recovery verification #2](https://github.com/rowan-smith/Vyrtel/issues/2) is completed; [ingest admission #3](https://github.com/rowan-smith/Vyrtel/issues/3) is in progress at this snapshot. See [Current Milestone](https://github.com/users/rowan-smith/projects/2/views/1) for live status and pickup priority.

## 0.1.3 — Complete everyday workflows

* Stable pagination and on-demand history loading while live tail is paused; clear empty/loading/error states; log/trace links, shareable URLs, saved queries, accessible keyboard workflows and fullscreen logs.
* A consistent square UI, one shared accessible search-bar component and a right-hand property drawer with selected/pinned log columns. Hide performance and memory diagnostics unless enabled in Profile.
* Mandatory UI login, local accounts, session invalidation, password changes and administrator account management; professional application settings and user profiles; per-signal retention controls.
* One global **all-API gate**, scoped keys, settings controls, stream reauthorization and safe legacy migration. Fresh installs default to gate **Off**, making **every application API ungated**. Gate **On** applies the same credential mechanism with operation scopes across all application APIs. UI login stays mandatory in both modes; when the gate is Off, UI login is not an API access boundary.
* Validate the existing operator-selected local/mounted storage directory; stage restart-based path changes in settings; verify and document complete stopped-server relocation. No live S3/MinIO backend or hot migration is included.
* Verify dashboard/alert workflows, expose metric aggregation limitations, and publish tested .NET/Serilog and OpenTelemetry Collector quickstarts using existing integrations.

See [Next Milestone](https://github.com/users/rowan-smith/projects/2/views/2). New native SDK packages belong to the separate 0.2.0 workstream.

## 0.2.0 — Faster log investigations

* Recoverable index sidecars, explicitly selected exact indexes, measured index recommendations and resource-bounded adaptive indexing; inspect index inventory and query cost.
* Define message-search semantics, then accelerate them with verified index candidates; membership lists and explicit wildcards. Regex remains discovery work.
* Direct text matching, bounded property/value suggestions and autocomplete in the search bar; remove the separate Filters tab.
* Canonical query functions such as `sum()` and `avg()`, numeric summaries, grouping and time buckets across supported log/span contexts, with syntax diagnostics and bounded execution.
* A friendly query builder opened by a **hammer icon on the left of the shared search bar**. Guided filters, summaries and groups round-trip through the canonical query model; unsupported expressions preserve their original text. Reuse these controls in later dashboard helpers.

See [the investigation milestone](https://github.com/rowan-smith/Vyrtel/milestone/3), including [the hammer-button builder #138](https://github.com/rowan-smith/Vyrtel/issues/138). Function availability follows each signal's capabilities; shared input does not mean every function works everywhere.

## 0.2.0 — Metrics correctness and interoperability

* Series identity and temporality; reset-aware cumulative rates, interval-aware delta aggregation, safe explicit histogram merging and approximate percentiles.
* Semantic aggregation selection, cardinality truncation feedback and metric calculations through the shared query language.
* OTLP/gRPC, a validated OpenAPI contract and interoperability fixtures using real OpenTelemetry SDKs.
* A common native SDK contract and reusable transport conformance harness; bounded log clients for **.NET, Node.js, Go, C++, Python and Java**, plus a Serilog sink reusing the .NET client. Standard OpenTelemetry recipes cover traces and metrics; native clients do not wait for future gRPC delivery.

SDK scope includes bounded in-memory queues, retries, flush/shutdown behaviour and compatibility documentation. Durable offline spooling and exactly-once producer delivery are not promised. Persistent client queues remain discovery work. See [the metrics and interoperability milestone](https://github.com/rowan-smith/Vyrtel/milestone/4).

## 0.2.1 — Retention and alert operations

* Preview and enforce selective retention within mixed segments, beyond the earlier per-signal settings.
* Persist notification delivery intent; inspect/retry failures; reuse destinations and deliver to Slack/Teams webhooks.
* Sustained alert conditions, maintenance silences, operational health and outbound webhook network controls.

See [the retention and alert milestone](https://github.com/rowan-smith/Vyrtel/milestone/5). Email delivery remains discovery work.

## 0.2.1 — Privacy and user audit trails

* Versioned PII rules, explicit masking semantics and bounded detection of configured sensitive values.
* Mask accepted telemetry **before persistence and live delivery**; settings for policy management and synthetic previews; verify reads, suggestions, exports and derived surfaces cannot reveal original masked values.
* A bounded append-only application audit stream for login/account/key actions and settings/dashboard/alert mutations; browse and export the user audit trail.

Masking is prospective, not a promise to rewrite historical data or detect all possible PII. Audit records exclude secrets, identify verified actors where available and distinguish unauthenticated activity when the global API gate is Off. This is not a tamper-proof compliance archive. All new public application APIs follow the same global gate/scope policy. See [the privacy and audit milestone](https://github.com/rowan-smith/Vyrtel/milestone/10).

## 0.2.2 — Dashboard investigation workflows

* Creation/editing through a toolbox, canvas and inspector; move/add/resize widgets with pointer and keyboard controls.
* **1 × 1, 2 × 2, 3 × 3 and 4 × 4 grid presets**, versioned geometry, legacy layout migration and non-destructive preset previews.
* Atomic draft saving with revision conflicts; shared search/query editing and simple helpers reusing the canonical guided controls.
* Multiple independently configured chart/metric series, safe rendering/refresh, per-widget duration overrides, shared dashboard time ranges, typed variables and independent error/empty states.
* Versioned saved/shared queries, dashboard import/export and drill-through to the source investigation.

See [the dashboard milestone](https://github.com/rowan-smith/Vyrtel/milestone/6). This depends on 0.2.0 query/metric semantics and shared guided controls rather than introducing a second query language.

## 0.2.3 — Security investigation workflows

* A minimal normalized security-event field contract.
* An editable **SIEM starter dashboard**, opt-in detection/alert recipes and evidence pivots into bounded log investigations.

This follows privacy/audit foundations and dashboard/alert capabilities. It is a focused security investigation starter, not a commitment to endpoint agents, a full correlation engine or enterprise SIEM parity. See [the security milestone](https://github.com/rowan-smith/Vyrtel/milestone/11).

## 0.3.0 — Ingest and Core node foundation

* One executable with `all`, `core` and `ingest` roles; all-in-one stays supported.
* One exclusive Core owner for telemetry, metadata and maintenance. Ingest nodes normalize/forward bounded batches using a versioned internal protocol.
* Independently authenticated private node transport, bounded retries and Core-backed acknowledgements. Public Ingest/Core APIs share the global gate/scope policy; disabling that public gate does not disable private node authentication.
* Stable internal batch identities, serialized retries, recovered deduplication evidence and a bounded/observable deduplication window.
* Recovery verification with a 10,000-event acknowledgement ledger; parity/performance measurements against all-in-one.

See [the node foundation milestone](https://github.com/rowan-smith/Vyrtel/milestone/8). Internal batch deduplication does not imply unlimited exactly-once producer delivery. Multiple Data shards, standalone Query/Gateway/Control roles and replication are outside this foundation.

## 0.3.1 — Multi-node operational hardening

* Role-aware health and forwarding diagnostics.
* Static multi-Ingest deployments and safe rolling upgrades.
* Release artifacts gated on role compatibility and supported topology checks.

See [the multi-node operations milestone](https://github.com/rowan-smith/Vyrtel/milestone/9).

## Future — Discovery and architecture decisions

These tickets produce findings, measurements and go/no-go decisions before delivery work is committed:

* S3/MinIO cold-storage tiering, a persistent edge collector and a durable .NET client queue.
* OIDC/identity recovery, broader multi-user/RBAC requirements, replication and high availability.
* Email notification operations, exponential histogram fidelity, regex budgets and compare-period investigation.
* Standalone Query and Gateway/Control roles, multiple Data shards and query coordination without assuming replication.

See [the discovery milestone](https://github.com/rowan-smith/Vyrtel/milestone/7). Object storage, replication, enterprise identity and extra standalone roles are not promised in the preceding releases.

## How work is picked up and released

Use the project board for live priority and status. P0 comes before P1 and P2, but implementation dependencies and external prerequisites still apply. A ticket queued in Ready is not evidence that its dependencies have shipped. Implementations include the ticket's acceptance evidence, relevant tests and documentation updates; release validation remains a separate gate.

Update this page, the linked milestone and affected issue scopes together when release scope changes. Dates, story counts and estimates are not delivery guarantees.
