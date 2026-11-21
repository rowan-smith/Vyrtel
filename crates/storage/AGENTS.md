# Storage Rules

The WAL is authoritative. Storage correctness takes priority over throughput.

- Never acknowledge data until its configured durability guarantee is met and it is visible to queries.
- Preserve crash recovery, ordering, integrity checks and compatibility with existing on-disk formats.
- Keep buffers, writer queues, caches and maintenance work bounded.
- Make write failures observable; do not silently drop accepted events or report uncertain writes as safe.
- Treat fsync, truncation, rename and cleanup ordering as correctness-sensitive operations.
- Test process crashes, disk/write faults, restart recovery and old-format reads when relevant.

Read `docs/storage-format.md` and `docs/architecture.md` before changing the write path.
