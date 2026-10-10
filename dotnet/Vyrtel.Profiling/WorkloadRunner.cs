using System.Diagnostics;
using Vyrtel.Harness;

namespace Vyrtel.Profiling;

public sealed record WorkloadRun(
    double DurationSeconds,
    int Concurrency,
    long TotalRequests,
    long TotalFailures,
    IReadOnlyList<WorkloadOpResult> Operations);

public sealed record WorkloadOpResult(WorkloadOp Op, LatencyStats Latency, long Failures);

/// <summary>
/// Runs the mixed workload until the deadline: several worker tasks time each operation
/// with a per-worker HTTP client, and failures are counted separately so tail percentiles
/// are never polluted by rejected requests.
/// </summary>
public static class WorkloadRunner
{
    public static async Task<WorkloadRun> RunAsync(
        string baseUrl,
        IReadOnlyList<WorkloadOp> ops,
        int concurrency,
        TimeSpan duration,
        CancellationToken ct = default)
    {
        var totalWeight = ops.Sum(o => o.Weight);
        var recorders = ops.ToDictionary(o => o.Name, o => new LatencyRecorder());
        var failures = ops.ToDictionary(o => o.Name, o => 0L);
        var failuresGate = new object();
        var deadline = DateTime.UtcNow + duration;

        WorkloadOp Pick(Random rng)
        {
            var roll = rng.Next(totalWeight);
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
                    await op.Run(api, "");
                    recorders[op.Name].Record(sw.Elapsed.TotalMilliseconds);
                }
                catch
                {
                    lock (failuresGate)
                    {
                        failures[op.Name]++;
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
            concurrency,
            results.Sum(r => r.Latency.Count),
            results.Sum(r => r.Failures),
            results);
    }
}