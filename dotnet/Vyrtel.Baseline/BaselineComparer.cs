namespace Vyrtel.Baseline;

/// <summary>
/// Compares a new report against a baseline using the baseline's proposed budgets. A comparison
/// passes only when it could actually judge something: no metric regressed, no metric the
/// baseline gated is missing, at least one metric was gated, and both reports measured the same
/// thing (configuration, workload, host, build profile, report schema). Anything else fails, so
/// a broken or incomparable run can never look like a pass.
/// </summary>
public static class BaselineComparer
{
    public const int ExitPassed = 0;
    public const int ExitFailed = 2;

    public static ComparisonResult Compare(BaselineReport baseline, BaselineReport current)
    {
        var currentByKey = current.Summary.ToDictionary(m => m.Key);
        var metrics = new List<MetricComparison>();
        foreach (var b in baseline.Summary)
        {
            if (!currentByKey.TryGetValue(b.Key, out var c))
            {
                metrics.Add(new MetricComparison(b.Key, b.Label, b.Unit, b.Mean, null, null, b.BudgetPercent, Verdict.Missing));
                continue;
            }
            var worse = Worse(b, c.Mean);
            metrics.Add(new MetricComparison(b.Key, b.Label, b.Unit, b.Mean, c.Mean, worse, b.BudgetPercent, Judge(worse, b.BudgetPercent)));
        }

        return new ComparisonResult(baseline.Label, baseline.GeneratedAt, baseline.VyrtelVersion, Incompatibilities(baseline, current), metrics);
    }

    /// <summary>
    /// How much worse the current mean is: percent of the baseline mean, or for a
    /// <see cref="MetricKind.Ratio"/> the difference in percentage points (so a 0% baseline
    /// still compares). Null when a relative change is undefined.
    /// </summary>
    public static double? Worse(MetricSummary baseline, double current)
    {
        var delta = baseline.Better == Better.Lower ? current - baseline.Mean : baseline.Mean - current;
        if (baseline.Kind == MetricKind.Ratio)
        {
            return delta;
        }
        return baseline.Mean == 0 ? null : delta / Math.Abs(baseline.Mean) * 100;
    }

    public static Verdict Judge(double? worsePercent, double? budgetPercent) =>
        (worsePercent, budgetPercent) switch
        {
            (_, null) => Verdict.NotGated,
            (null, _) => Verdict.Regression, // gated, but the change cannot be measured
            var (w, b) when w > b => Verdict.Regression,
            var (w, b) when -w > b => Verdict.Improvement,
            _ => Verdict.WithinBudget,
        };

    /// <summary>Differences that make the two reports measure different things. Any one fails the comparison.</summary>
    public static IReadOnlyList<string> Incompatibilities(BaselineReport baseline, BaselineReport current)
    {
        var found = new List<string>();
        void Check<T>(string what, T before, T after)
        {
            if (!EqualityComparer<T>.Default.Equals(before, after))
            {
                found.Add($"{what} differs: baseline {before}, current {after}");
            }
        }

        if (baseline.SchemaVersion != current.SchemaVersion)
        {
            found.Add($"report schema differs: baseline {baseline.SchemaVersion}, current {current.SchemaVersion}");
        }
        Check("pinned configuration (SHA-256)", baseline.Config.Sha256, current.Config.Sha256);
        // The number of runs may differ: a quick check against a longer baseline is legitimate.
        Check("workload", baseline.Workload with { Runs = 0 }, current.Workload with { Runs = 0 });
        Check("CPU", baseline.Host.CpuModel, current.Host.CpuModel);
        Check("logical cores", baseline.Host.LogicalCores, current.Host.LogicalCores);
        Check("operating system", baseline.Host.Os, current.Host.Os);
        Check("architecture", baseline.Host.Architecture, current.Host.Architecture);
        Check("build profile", baseline.Build.Profile, current.Build.Profile);
        return found;
    }

    /// <summary>The process exit status for a comparison: 0 only when it passed.</summary>
    public static int ExitCode(ComparisonResult result) => result.Passed ? ExitPassed : ExitFailed;
}
