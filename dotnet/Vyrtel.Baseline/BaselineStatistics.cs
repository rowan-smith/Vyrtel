using System.Globalization;

namespace Vyrtel.Baseline;

/// <summary>
/// One metric's value from one run. A percentile also carries the quantile and how many samples
/// it was taken from, because a tail estimated from a few samples is too fragile to gate on.
/// </summary>
public sealed record MetricSample(
    string Key, string Label, string Unit, Better Better, MetricKind Kind, double Value, double? Quantile = null, int? Samples = null,
    bool Informational = false);

/// <summary>
/// Turns repeated runs into per-metric spread and proposed regression budgets.
///
/// A budget is how much worse than the baseline mean a new mean may be before it is called a
/// regression. It is derived from the observed run-to-run noise, not chosen per metric:
/// <c>budget = max(floor, roundUp(3 × CV × k(n)))</c>. CV is the coefficient of variation
/// (sample standard deviation ÷ mean) across the n runs; <c>k(n)</c> turns it into a 90% upper
/// confidence bound on the true spread, because a handful of runs usually underestimates it
/// (k = 3.08 for 3 runs, 1.94 for 5, 1.47 for 10). Three of those standard deviations keeps false
/// alarms from noise rare. Fewer than <see cref="MinRunsForBudget"/> runs, a zero mean, a
/// percentile with fewer than <see cref="TailSamples"/> samples beyond it in some run, or a
/// budget over <see cref="MaxBudgetPercent"/> leave the metric ungated, with the reason recorded.
/// The floors come from same-commit comparisons on the reference laptop (docs/benchmarks.md).
/// </summary>
public static class BaselineStatistics
{
    public const int MinRunsForBudget = 3;
    public const double Sigmas = 3;
    public const double Confidence = 0.90;
    /// <summary>
    /// Timing floor: above the largest drift measured between baselines of the same commit
    /// recorded in different sessions on the reference laptop (11.2%), which within-session
    /// variance cannot see.
    /// </summary>
    public const double TimingFloorPercent = 15;

    /// <summary>Size floor: data-directory size varied 0.55% across 16 runs of the same commit; binaries not at all.</summary>
    public const double SizeFloorPercent = 2;

    /// <summary>
    /// Ratio floor, in percentage points: a share of unsuccessful requests may rise by this much
    /// before it is a regression. A policy choice: the reference laptop rejected nothing in 16 runs,
    /// so there was no spread to measure; one request in a hundred failing is already a change.
    /// </summary>
    public const double RatioFloorPoints = 1;

    /// <summary>A percentile is gated only when every run had at least this many samples beyond it.</summary>
    public const int TailSamples = 10;
    public const double MaxBudgetPercent = 50;

    // Lower 10% quantiles of the chi-square distribution for 1–30 degrees of freedom.
    private static readonly double[] ChiSquare10 =
    [
        0.01579, 0.2107, 0.5844, 1.0636, 1.6103, 2.2041, 2.8331, 3.4895, 4.1682, 4.8652,
        5.5778, 6.3038, 7.0415, 7.7895, 8.5468, 9.3122, 10.0852, 10.8649, 11.6509, 12.4426,
        13.2396, 14.0415, 14.8480, 15.6587, 16.4734, 17.2919, 18.1139, 18.9392, 19.7677, 20.5992,
    ];

    /// <summary>
    /// k(n) = √((n − 1) ÷ χ²₀.₁₀(n − 1)): the factor that turns a sample standard deviation from
    /// n runs into a 90% upper confidence bound on the true one (assuming roughly normal noise).
    /// </summary>
    public static double UpperBoundFactor(int runs)
    {
        if (runs < 2)
        {
            throw new ArgumentOutOfRangeException(nameof(runs), "a spread needs at least 2 runs");
        }
        var df = runs - 1;
        double chi;
        if (df <= ChiSquare10.Length)
        {
            chi = ChiSquare10[df - 1];
        }
        else
        {
            // Wilson–Hilferty approximation, accurate to well under 1% beyond 30 degrees of freedom.
            const double z10 = -1.2815516;
            var t = 2.0 / (9 * df);
            chi = df * Math.Pow(1 - t + z10 * Math.Sqrt(t), 3);
        }
        return Math.Sqrt(df / chi);
    }

    /// <summary>The metrics each run contributes to the summary.</summary>
    public static IEnumerable<MetricSample> Flatten(RunResult run, long executableBytes)
    {
        var s = run.Seed;
        yield return new("seed.unsuccessfulPercent", "Seeding requests not accepted (retried)", "%", Better.Lower, MetricKind.Ratio, Share(s.Requests));
        yield return new("seed.itemsPerSecond", "Seeding throughput", "items/s", Better.Higher, MetricKind.Timing, s.ItemsPerSecond);
        yield return new("seed.p95Ms", "Seeding request p95", "ms", Better.Lower, MetricKind.Timing, s.Latency.P95, 0.95, s.Latency.Count);

        foreach (var q in run.Queries)
        {
            yield return new($"query.{q.Name}.coldMs", $"{q.Label}: cold", "ms", Better.Lower, MetricKind.Timing, q.ColdMs);
            yield return new($"query.{q.Name}.warmP50Ms", $"{q.Label}: warm p50", "ms", Better.Lower, MetricKind.Timing, q.Warm.P50, 0.50, q.Warm.Count);
            yield return new($"query.{q.Name}.warmP95Ms", $"{q.Label}: warm p95", "ms", Better.Lower, MetricKind.Timing, q.Warm.P95, 0.95, q.Warm.Count);
        }

        var m = run.Mixed;
        var measured = Math.Max(m.MeasuredSeconds, 0.001);
        // Only successful work counts as throughput: a server answering 429 quickly must not look faster.
        yield return new("mixed.acceptedPerSecond", "Mixed load successful requests", "req/s", Better.Higher, MetricKind.Timing, m.Requests.Accepted / measured);
        yield return new("mixed.ingestEventsPerSecond", "Mixed load accepted ingest events", "events/s", Better.Higher, MetricKind.Timing, m.EventsAccepted / measured);
        yield return new("mixed.unsuccessfulPercent", "Mixed load requests not accepted", "%", Better.Lower, MetricKind.Ratio, Share(m.Requests));
        yield return new("mixed.requestsPerSecond", "Mixed load attempted requests", "req/s", Better.Higher, MetricKind.Timing, m.RequestsPerSecond, Informational: true);
        foreach (var op in m.Operations.Where(o => o.Latency.Count > 0))
        {
            var n = op.Latency.Count;
            yield return new($"mixed.{op.Name}.p50Ms", $"Mixed {op.Label}: p50", "ms", Better.Lower, MetricKind.Timing, op.Latency.P50, 0.50, n);
            yield return new($"mixed.{op.Name}.p95Ms", $"Mixed {op.Label}: p95", "ms", Better.Lower, MetricKind.Timing, op.Latency.P95, 0.95, n);
            yield return new($"mixed.{op.Name}.p99Ms", $"Mixed {op.Label}: p99", "ms", Better.Lower, MetricKind.Timing, op.Latency.P99, 0.99, n);
        }

        yield return new("memory.peakRssBytes", "Peak RSS", "bytes", Better.Lower, MetricKind.Timing, run.Memory.PeakRssBytes);
        yield return new("server.restartMs", "Restart to healthy (mean)", "ms", Better.Lower, MetricKind.Timing, run.MeanRestartMs);
        yield return new("storage.dataDirBytes", "Data directory after seeding", "bytes", Better.Lower, MetricKind.Size, run.DataDirBytes);
        if (run.Storage is { EventCount: > 0 } storage)
        {
            yield return new("storage.bytesPerEvent", "Stored bytes per event", "bytes", Better.Lower, MetricKind.Size, storage.BytesStoredPerEvent);
        }
        if (executableBytes > 0)
        {
            yield return new("build.executableBytes", "Binary size", "bytes", Better.Lower, MetricKind.Size, executableBytes);
        }
    }

    /// <summary>Backpressure, rejections and transport errors as a percentage of all requests.</summary>
    public static double Share(RequestCounts c) => c.Total > 0 ? (c.Total - c.Accepted) * 100.0 / c.Total : 0;

    public static IReadOnlyList<MetricSummary> Summarize(IReadOnlyList<RunResult> runs, long executableBytes) =>
        Summarize(runs.SelectMany(r => Flatten(r, executableBytes)));

    public static IReadOnlyList<MetricSummary> Summarize(IEnumerable<MetricSample> samples) =>
        samples
            .GroupBy(s => s.Key)
            .Select(g =>
            {
                var first = g.First();
                var fewest = g.Any(s => s.Samples is null) ? (int?)null : g.Min(s => s.Samples!.Value);
                return Summarize(first.Key, first.Label, first.Unit, first.Better, first.Kind, g.Select(s => s.Value).ToList(), first.Quantile, fewest, first.Informational);
            })
            .ToList();

    public static MetricSummary Summarize(
        string key, string label, string unit, Better better, MetricKind kind, IReadOnlyList<double> values,
        double? quantile = null, int? fewestSamples = null, bool informational = false)
    {
        var n = values.Count;
        var mean = n > 0 ? values.Average() : 0;
        var stdDev = n > 1 ? Math.Sqrt(values.Sum(v => (v - mean) * (v - mean)) / (n - 1)) : 0;
        double? cv = mean != 0 && n > 1 ? stdDev / Math.Abs(mean) * 100 : null;
        var (budget, why) = informational
            ? (null, "informational: counts rejected responses too, so it is never gated (see successful requests)")
            : kind == MetricKind.Ratio
                ? ProposeRatioBudget(n, stdDev)
                : ProposeBudget(n, mean, cv, kind);
        if (budget is not null && quantile is { } q && fewestSamples is { } fewest && fewest < RequiredSamples(q))
        {
            (budget, why) = (null, string.Create(CultureInfo.InvariantCulture,
                $"not gated: p{q * 100:0} from {fewest} samples in one run; at least {RequiredSamples(q)} are needed for {TailSamples} beyond it"));
        }
        return new MetricSummary(
            key, label, unit, better, kind, n, mean, stdDev, cv,
            n > 0 ? values.Min() : 0,
            n > 0 ? values.Max() : 0,
            budget, why, values);
    }

    /// <summary>Samples a run needs for <see cref="TailSamples"/> of them to lie beyond quantile <paramref name="q"/>: 20 for p50, 200 for p95, 1,000 for p99.</summary>
    public static int RequiredSamples(double q) => (int)Math.Ceiling(TailSamples / (1 - q) - 1e-9);

    /// <summary>
    /// Budget for a <see cref="MetricKind.Ratio"/>, in percentage points rather than relative to the
    /// mean, so a baseline with no failures (mean 0) is still gated: 3 × the upper-bound standard
    /// deviation, rounded up to a whole point, at least <see cref="RatioFloorPoints"/>.
    /// </summary>
    public static (double? Budget, string Justification) ProposeRatioBudget(int runs, double stdDevPoints)
    {
        if (runs < MinRunsForBudget)
        {
            return (null, $"not gated: {runs} run(s); at least {MinRunsForBudget} are needed to estimate run-to-run variance");
        }
        var k = UpperBoundFactor(runs);
        var scaled = Sigmas * stdDevPoints * k;
        var rounded = Math.Ceiling(scaled - 1e-9);
        var budget = Math.Max(RatioFloorPoints, rounded);
        var arithmetic = string.Create(CultureInfo.InvariantCulture,
            $"{runs} runs, std dev {stdDevPoints:0.00} points → 3 × upper bound (×{k:0.00}) = {scaled:0.0} points, rounded up to {rounded:0}; floor {RatioFloorPoints:0} point");
        if (budget > MaxBudgetPercent)
        {
            return (null, $"not gated: {arithmetic} → {budget:0} points exceeds {MaxBudgetPercent:0}; too noisy to gate");
        }
        return (budget, $"{arithmetic} → {budget:0} points");
    }

    /// <summary>The proposed budget in percent (null = not gated) and the arithmetic behind it.</summary>
    public static (double? Budget, string Justification) ProposeBudget(int runs, double mean, double? cvPercent, MetricKind kind)
    {
        if (runs < MinRunsForBudget)
        {
            return (null, $"not gated: {runs} run(s); at least {MinRunsForBudget} are needed to estimate run-to-run variance");
        }
        if (mean == 0 || cvPercent is null)
        {
            return (null, "not gated: the mean is zero, so a relative budget is undefined");
        }

        var cv = cvPercent.Value;
        var floor = kind == MetricKind.Size ? SizeFloorPercent : TimingFloorPercent;
        var step = kind == MetricKind.Size ? 1 : 5;
        var k = UpperBoundFactor(runs);
        var upper = cv * k;
        var scaled = Sigmas * upper;
        var rounded = Math.Ceiling(scaled / step) * step;
        var budget = Math.Max(floor, rounded);
        var arithmetic = string.Create(CultureInfo.InvariantCulture,
            $"{runs} runs, CV {cv:0.0}% → 90% upper bound {upper:0.0}% (×{k:0.00}) → 3× = {scaled:0.0}%, rounded up to {rounded:0}%; floor {floor:0}%");
        if (budget > MaxBudgetPercent)
        {
            return (null, $"not gated: {arithmetic} → {budget:0}% exceeds {MaxBudgetPercent:0}%; too noisy to gate with this many runs on this host");
        }
        return (budget, $"{arithmetic} → {budget:0}%");
    }
}
