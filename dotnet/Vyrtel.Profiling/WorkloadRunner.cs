using System.Diagnostics;
using Vyrtel.Harness;

namespace Vyrtel.Profiling;

public sealed record WorkloadRun(
    double DurationSeconds,
    double WarmupSeconds,
    double MeasurementSeconds,
    int Concurrency,
    long TotalRequests,
    long TotalFailures,
    IReadOnlyList<WorkloadOpResult> Operations);

public sealed record WorkloadOpResult(WorkloadOp Op, LatencyStats Latency, long Failures);

/// <summary>
/// Drives the mixed workload for <paramref name="duration"/>, discarding the first
/// <paramref name="warmup"/> so percentiles reflect steady state (JIT, connection pool and
/// segment-cache ramp-up all settle first). Each worker keeps its own connection so the
/// reported latency is per-request, not per-shared-connection queueing.
/// </summary>
public sealed class WorkloadRunner
{
    public static async Task<WorkloadRun> RunAsync(
        string baseUrl,
        IReadOnlyList<WorkloadOp> ops,
        int concurrency,
        TimeSpan duration,
        TimeSpan warmup,
        CancellationToken ct = default)
    {
        if (warmup >= duration)
        {
            warmup = TimeSpan.Zero;
        }

        var total = ops.Sum(o => o.Weight);
        var recorders = ops.ToDictionary(o => o.Name, _ => new LatencyRecorder());
        var failures = ops.ToDictionary(o => o.Name, _ => 0L);
        var failuresGate = new object();

        var start = DateTime.UtcNow;
        var recordFrom = start + warmup;
        var deadline = start + duration;

        WorkloadOp Pick(Random rng)
        {
            var roll = rng.Next(total);
            foreach (var op in ops)
            {
                roll -= op.Weight;
                if (roll < 0)
                {
                    return op;
                }
            }
            return ops[^1];
        }

        var workers = Enumerable.Range(0, concurrency).Select(async _ =>
        {
            using var api = new ApiClient(baseUrl);
            var rng = new Random();
            while (DateTime.UtcNow < deadline && !ct.IsCancellationRequested)
            {
                var op = Pick(rng);
                var sw = Stopwatch.StartNew();
                try
                {
                    await op.Run(api);
                    var elapsed = sw.Elapsed.TotalMilliseconds;
                    if (DateTime.UtcNow >= recordFrom)
                    {
                        recorders[op.Name].Record(elapsed);
                    }
                }
                catch
                {
                    if (DateTime.UtcNow >= recordFrom)
                    {
                        lock (failuresGate)
                        {
                            failures[op.Name]++;
                        }
                    }
                }
            }
        });

        await Task.WhenAll(workers);

        var results = ops
            .Select(op => new WorkloadOpResult(op, recorders[op.Name].Snapshot(), failures[op.Name]))
            .OrderByDescending(r => r.Op.Weight)
            .ToList();

        return new WorkloadRun(
            duration.TotalSeconds,
            warmup.TotalSeconds,
            (duration - warmup).TotalSeconds,
            concurrency,
            results.Sum(r => r.Latency.Count),
            results.Sum(r => r.Failures),
            results);
    }
}
