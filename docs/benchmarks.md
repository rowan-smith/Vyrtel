# Benchmarks

Benchmarks are run by hand on fixed hardware, following the [benchmark method](benchmark-method.md)
so that anyone can repeat them. Results are added to this page once a run is complete and reviewed.

> **No results published yet.** The first results will appear here once the method has been run
> against a release.

<!--
To publish a run, fill in each section below and remove the notice above. Write "not run" for
anything that was not measured; never estimate. Keep earlier releases in the history table so
changes between versions stay visible.
-->

## Setup

| | Primary (runs Vyrtel) | Secondary (load only) |
|---|---|---|
| Hardware | _CPU, cores, memory, disk_ | _CPU, cores, memory_ |
| Operating system | _OS and version_ | _OS and version_ |
| Build | _Vyrtel version, commit, binary hash or image digest_ | _load generator commit and hash_ |
| Network | | _private network or SSH tunnel_ |
| Configuration | _defaults, or what was changed_ | |
| Workload | _event shape, seed, rates, durations and repeats_ | |

## Results

Local results are measured on the primary alone; remote results have load sent from the secondary.
They are never mixed.

| Metric | Previous | This release | Verdict |
|---|---:|---:|---|
| Local ingest throughput (events/s) | | | |
| Local query p95 (ms) | | | |
| Local peak memory (MiB) | | | |
| Local compression ratio | | | |
| Remote ingest throughput (events/s) | | | |
| Remote ingest p95 / p99 (ms) | | | |
| Soak (hours, data loss, memory trend) | | | |

**Integrity:** offered _n_, acknowledged _n_, stored _n_; unexplained errors or data loss: _none_.
**Limitations and untested areas:** _list them_.

## History

| Version | Date | Verdict | Local ingest | Remote ingest | Local query p95 | Notes |
|---|---|---|---|---|---|---|
| | | | | | | |

Numbers depend on hardware and data shape. Compare them between versions, not with your own
machine.
