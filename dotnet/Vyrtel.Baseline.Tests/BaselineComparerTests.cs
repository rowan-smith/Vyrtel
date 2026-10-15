namespace Vyrtel.Baseline.Tests;

/// <summary>Comparing a report against a baseline's budgets, and refusing to hide setup differences.</summary>
public class BaselineComparerTests
{
    private static readonly BaselineReport Baseline =
        Fixtures.Report([Fixtures.Run(1, 1.0), Fixtures.Run(2, 1.01), Fixtures.Run(3, 0.99)]);

    [Theory]
    [InlineData(5.0, 10.0, Verdict.WithinBudget)]
    [InlineData(10.0, 10.0, Verdict.WithinBudget)] // at the budget is not over it
    [InlineData(10.1, 10.0, Verdict.Regression)]
    [InlineData(-10.1, 10.0, Verdict.Improvement)]
    public void Judges_against_the_budget(double worse, double budget, Verdict expected) =>
        Assert.Equal(expected, BaselineComparer.Judge(worse, budget));

    [Fact]
    public void Ungated_metrics_are_never_regressions() =>
        Assert.Equal(Verdict.NotGated, BaselineComparer.Judge(500, null));

    [Fact]
    public void Slower_latency_beyond_budget_is_a_regression()
    {
        var current = Fixtures.Report([Fixtures.Run(1, 1.2), Fixtures.Run(2, 1.2), Fixtures.Run(3, 1.2)]);

        var cmp = BaselineComparer.Compare(Baseline, current);

        var cold = cmp.Metrics.Single(m => m.Key == "query.logs.traceId.coldMs");
        Assert.Equal(Verdict.Regression, cold.Verdict);
        Assert.Equal(20, cold.WorsePercent!.Value, 6);
        // Throughput is higher-is-better: dividing it by 1.2 is also worse.
        var throughput = cmp.Metrics.Single(m => m.Key == "mixed.requestsPerSecond");
        Assert.Equal(Verdict.Regression, throughput.Verdict);
        Assert.True(cmp.Regressions > 0);
    }

    [Fact]
    public void Faster_beyond_budget_is_an_improvement_not_a_regression()
    {
        var current = Fixtures.Report([Fixtures.Run(1, 0.7), Fixtures.Run(2, 0.7), Fixtures.Run(3, 0.7)]);

        var cmp = BaselineComparer.Compare(Baseline, current);

        Assert.Equal(Verdict.Improvement, cmp.Metrics.Single(m => m.Key == "query.logs.traceId.coldMs").Verdict);
        Assert.Equal(Verdict.Improvement, cmp.Metrics.Single(m => m.Key == "mixed.requestsPerSecond").Verdict);
    }

    [Fact]
    public void Noise_within_the_baselines_spread_passes()
    {
        var current = Fixtures.Report([Fixtures.Run(1, 1.03), Fixtures.Run(2, 1.04), Fixtures.Run(3, 1.02)]);

        var cmp = BaselineComparer.Compare(Baseline, current);

        Assert.Equal(0, cmp.Regressions);
        Assert.Empty(cmp.Warnings);
    }

    [Fact]
    public void Peak_rss_growth_is_caught()
    {
        var current = Fixtures.Report([Fixtures.Run(1, peakRss: 80 << 20), Fixtures.Run(2, peakRss: 80 << 20), Fixtures.Run(3, peakRss: 80 << 20)]);

        var cmp = BaselineComparer.Compare(Baseline, current);

        Assert.Equal(Verdict.Regression, cmp.Metrics.Single(m => m.Key == "memory.peakRssBytes").Verdict);
    }

    [Fact]
    public void A_metric_missing_from_the_current_report_is_flagged()
    {
        var current = Baseline with { Summary = Baseline.Summary.Where(m => m.Key != "memory.peakRssBytes").ToList() };

        var cmp = BaselineComparer.Compare(Baseline, current);

        Assert.Equal(Verdict.Missing, cmp.Metrics.Single(m => m.Key == "memory.peakRssBytes").Verdict);
    }

    [Fact]
    public void A_short_baseline_gates_nothing_and_says_so()
    {
        var shortBaseline = Fixtures.Report([Fixtures.Run(1), Fixtures.Run(2)]);
        var current = Fixtures.Report([Fixtures.Run(1, 2), Fixtures.Run(2, 2)]);

        var cmp = BaselineComparer.Compare(shortBaseline, current);

        Assert.Equal(0, cmp.Regressions);
        Assert.All(cmp.Metrics, m => Assert.Equal(Verdict.NotGated, m.Verdict));
        Assert.Contains(cmp.Warnings, w => w.Contains("2 run(s)"));
    }

    [Fact]
    public void Different_configuration_or_host_is_warned_about()
    {
        var current = Fixtures.Report([Fixtures.Run(1), Fixtures.Run(2), Fixtures.Run(3)], configSha: new string('f', 64), cpu: "Other CPU");

        var warnings = BaselineComparer.Compare(Baseline, current).Warnings;

        Assert.Contains(warnings, w => w.StartsWith("pinned configuration"));
        Assert.Contains(warnings, w => w.StartsWith("CPU differs"));
    }

    [Fact]
    public void A_different_number_of_runs_is_not_a_workload_change()
    {
        var current = Fixtures.Report([Fixtures.Run(1), Fixtures.Run(2), Fixtures.Run(3), Fixtures.Run(4), Fixtures.Run(5)]);

        Assert.Empty(BaselineComparer.Compare(Baseline, current).Warnings);
    }

    [Fact]
    public void A_different_seed_is_a_workload_change()
    {
        var current = Baseline with { Workload = Baseline.Workload with { Seed = 7 } };

        Assert.Contains(BaselineComparer.Compare(Baseline, current).Warnings, w => w.StartsWith("workload differs"));
    }
}
