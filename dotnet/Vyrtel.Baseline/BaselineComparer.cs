namespace Vyrtel.Baseline;

/// <summary>
/// Compares a new report against a baseline using the baseline's proposed budgets. Differences
/// in what was measured (configuration, workload, host, build profile) are reported as
/// warnings, since a regression across different setups says nothing about the code.
/// </summary>
public static class BaselineComparer
{
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
            double? worse = b.Mean == 0
                ? null
                : (b.Better == Better.Lower ? c.Mean - b.Mean : b.Mean - c.Mean) / Math.Abs(b.Mean) * 100;
            metrics.Add(new MetricComparison(b.Key, b.Label, b.Unit, b.Mean, c.Mean, worse, b.BudgetPercent, Judge(worse, b.BudgetPercent)));
        }

        return new ComparisonResult(baseline.Label, baseline.GeneratedAt, baseline.VyrtelVersion, Warnings(baseline, current), metrics);
    }

    public static Verdict Judge(double? worsePercent, double? budgetPercent) =>
        (worsePercent, budgetPercent) switch
        {
            (null, _) or (_, null) => Verdict.NotGated,
            var (w, b) when w > b => Verdict.Regression,
            var (w, b) when -w > b => Verdict.Improvement,
            _ => Verdict.WithinBudget,
        };

    public static IReadOnlyList<string> Warnings(BaselineReport baseline, BaselineReport current)
    {
        var warnings = new List<string>();
        void Check<T>(string what, T before, T after)
        {
            if (!EqualityComparer<T>.Default.Equals(before, after))
            {
                warnings.Add($"{what} differs: baseline {before}, current {after}");
            }
        }

        Check("pinned configuration (SHA-256)", baseline.Config.Sha256, current.Config.Sha256);
        // The number of runs may differ: a quick check against a longer baseline is legitimate.
        Check("workload", baseline.Workload with { Runs = 0 }, current.Workload with { Runs = 0 });
        Check("CPU", baseline.Host.CpuModel, current.Host.CpuModel);
        Check("logical cores", baseline.Host.LogicalCores, current.Host.LogicalCores);
        Check("operating system", baseline.Host.Os, current.Host.Os);
        Check("architecture", baseline.Host.Architecture, current.Host.Architecture);
        Check("build profile", baseline.Build.Profile, current.Build.Profile);
        if (baseline.SchemaVersion != current.SchemaVersion)
        {
            warnings.Add($"report schema differs: baseline {baseline.SchemaVersion}, current {current.SchemaVersion}");
        }
        if (baseline.Runs.Count < BaselineStatistics.MinRunsForBudget)
        {
            warnings.Add($"the baseline has {baseline.Runs.Count} run(s), so no metric is gated; record at least {BaselineStatistics.MinRunsForBudget}");
        }
        return warnings;
    }
}
