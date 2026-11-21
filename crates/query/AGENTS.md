# Query Rules

Queries must be correct, cancellable and resource-bounded.

- Keep parsing and query syntax separate from storage execution.
- Indexes and pruning may skip only data proven irrelevant; optimisations must not change results.
- Respect shared time, scan-byte and concurrency budgets, including after the scan.
- Stop work on cancellation and avoid unbounded result sets or per-query allocations.
- Keep ordering, pagination and diagnostics consistent with the public API.
- Test indexed vs non-indexed results, boundary expressions, cancellation and exhausted budgets.

Read `docs/query-language.md` before changing language semantics.
