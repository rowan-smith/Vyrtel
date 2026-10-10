namespace Vyrtel.Harness;

/// <summary>
/// Thread-safe collector of latency samples (milliseconds) with percentile snapshots.
/// Used by the profiler to rank operations by tail latency.
/// </summary>
public sealed class LatencyRecorder
{
    private readonly object _gate = new();
    private readonly List<double> _samples = [];

    public int Count
    {
        get
        {
            lock (_gate)
            {
                return _samples.Count;
            }
        }
    }

    public void Record(double milliseconds)
    {
        lock (_gate)
        {
            _samples.Add(milliseconds);
        }
    }

    public LatencyStats Snapshot()
    {
        double[] sorted;
        lock (_gate)
        {
            sorted = [.. _samples];
        }
        Array.Sort(sorted);
        return LatencyStats.From(sorted);
    }

    public void Reset()
    {
        lock (_gate)
        {
            _samples.Clear();
        }
    }
}

/// <summary>Summary statistics for a set of latency samples, in milliseconds.</summary>
public readonly record struct LatencyStats(
    int Count,
    double Min,
    double Max,
    double Mean,
    double P50,
    double P90,
    double P95,
    double P99,
    double StdDev)
{
    public static readonly LatencyStats Empty = new(0, 0, 0, 0, 0, 0, 0, 0, 0);

    public static LatencyStats From(double[] sorted)
    {
        if (sorted.Length == 0)
        {
            return Empty;
        }

        var mean = 0.0;
        foreach (var value in sorted)
        {
            mean += value;
        }
        mean /= sorted.Length;

        var variance = 0.0;
        foreach (var value in sorted)
        {
            var diff = value - mean;
            variance += diff * diff;
        }
        variance = sorted.Length > 1 ? variance / (sorted.Length - 1) : 0;

        return new LatencyStats(
            sorted.Length,
            sorted[0],
            sorted[^1],
            mean,
            Percentile(sorted, 50),
            Percentile(sorted, 90),
            Percentile(sorted, 95),
            Percentile(sorted, 99),
            Math.Sqrt(variance));
    }

    private static double Percentile(double[] sorted, double percentile)
    {
        if (sorted.Length == 0)
        {
            return 0;
        }
        var rank = (int)Math.Ceiling(percentile / 100.0 * sorted.Length) - 1;
        return sorted[Math.Clamp(rank, 0, sorted.Length - 1)];
    }
}
