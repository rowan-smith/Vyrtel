
### mixed-high

| Metric | Key median | Session median | Change | Key runs | Session runs |
|---|---:|---:|---:|---|---|
| requests/s | 3.25 | 3.23 | -0.5% | 3.25 | 3.23 |
| load p50 ms | 2942 | 2636 | -10.4% | 2942 | 2636 |
| load p95 ms | 9959 | 9957 | -0.0% | 9959 | 9957 |
| load p99 ms | 9996 | 9995 | -0.0% | 9996 | 9995 |
| load max ms | 10000 | 9999 | -0.0% | 10000 | 9999 |
| cold load p99 ms | 1848 | 2206 | +19.4% | 1848 | 2206 |
| search p99 ms | 9965 | 9954 | -0.1% | 9965 | 9954 |
| facets p99 ms | 9998 | 9996 | -0.0% | 9998 | 9996 |
| dashboard p99 ms | 9750 | 9737 | -0.1% | 9750 | 9737 |
| probe p50 ms | 0.34 | 0.39 | +15.5% | 0.34 | 0.39 |
| probe p99 ms | 1.51 | 1.90 | +26.2% | 1.51 | 1.90 |
| probe max ms | 11.83 | 54.53 | +360.9% | 11.83 | 54.53 |
| heartbeat p99 ms | 1.53 | 1.78 | +16.8% | 1.53 | 1.78 |
| 503s | 0.00 | 0.00 | – | 0.00 | 0.00 |
| ingest events/s | 102166 | 92752 | -9.2% | 102166 | 92752 |
| ingest 429s | 27125 | 27882 | +2.8% | 27125 | 27882 |
| write pairs | 4455 | 4522 | +1.5% | 4455 | 4522 |
| cache hits | 0.00 | 33741 | – | 0.00 | 33741 |
| lookups/s | 0.00 | 0.00 | – | 0.00 | 0.00 |
| lookup queue µs | – | – | – | – | – |
| lookup lock µs | – | – | – | – | – |
| lookup query µs | – | – | – | – | – |
| lookup max lock µs | 0.00 | 0.00 | – | 0.00 | 0.00 |
| lookups in flight (peak) | 1.00 | 1.00 | +0.0% | 1.00 | 1.00 |
| CPU µs/request | 911211 | 976829 | +7.2% | 911211 | 976829 |
| peak MiB | 458 | 486 | +6.0% | 458 | 486 |

Not met:
- mixed-high key run1: load {"ok":801,"unauthorized401":0,"forbidden403":0,"rejected429":0,"unavailable503":0,"serverError5xx":0,"otherStatus":0,"timeouts":494,"transportErrors":0}
- mixed-high session run1: load {"ok":779,"unauthorized401":0,"forbidden403":0,"rejected429":0,"unavailable503":0,"serverError5xx":0,"otherStatus":0,"timeouts":469,"transportErrors":0}
