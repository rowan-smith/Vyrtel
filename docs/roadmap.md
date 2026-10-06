# Roadmap

Keeping future work written down here keeps it out of the v0.1 code.

## v0.1 (this release)

* Single binary, single data directory, no external services.
* Native JSON/NDJSON ingestion (CLEF-compatible) and OTLP/HTTP (protobuf and
  JSON) for logs, traces and metrics.
* Append-only WAL with CRC-checked records; `normal` and `strict` durability.
* Immutable, versioned, block-compressed (zstd) segments with time ranges,
  bitmap indexes, Bloom filters, zone maps and field statistics.
* Crash recovery, compaction of small segments, per-signal retention.
* Query language with `= != > >= < <= contains and or not ()`, nested fields,
  type coercion; cursor pagination; query diagnostics.
* Live tail over SSE with backpressure.
* Trace search, trace view (waterfall) and log ↔ trace correlation.
* Basic metrics explorer (sum/count/min/max/avg/last, group by).
* Dashboards (log count, metric line, single stat, saved query panels).
* Log-count threshold alerts with webhook delivery.
* API keys, admin login, storage statistics, bounded memory.
* Query statistics recorded for future adaptive indexing.

## v0.2 (next)

* **Adaptive indexing** driven by the recorded query statistics: build exact
  per-segment indexes for frequently queried high-cardinality fields
  (stored under `data/indexes/`, no segment format change needed).
* Full-text token index for `message contains`.
* Retention policies by level, service and event type.
* OTLP/gRPC.
* Alert routing and more notification targets (email, Slack, Teams).
* Better metrics: rates for cumulative counters, percentiles from
  histograms, a small metrics-specific query syntax (not PromQL).
* Dashboard variables and drag-and-drop layout.
* OpenAPI document generated from the server.
* `in (...)` lists and wildcards in the query language.

## Future

* Local → object-storage tiering (S3/MinIO) for cold segments.
* Agent/collector binary with a persistent edge queue.
* Distributed query and ingest nodes, storage replication.
* Compare-period investigation and automatic change detection.
* OIDC/SAML/LDAP and role-based access control.

## Explicit non-goals for v0.1

Distributed clustering, consensus, replication, multi-region, Kafka, an
external SQL database, S3 cold storage, full PromQL or SQL, machine
learning/LLM features, automatic index creation, advanced dashboards or
alert routing, enterprise identity, mobile apps, Kubernetes operators,
plugin marketplace, billing and licensing.
