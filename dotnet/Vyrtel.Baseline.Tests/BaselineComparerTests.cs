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
    public void A_gated_change_that_cannot_be_measured_fails() =>
        Assert.Equal(Verdict.Regression, BaselineComparer.Judge(null, 10));

    [Fact]
    public void Slower_latency_beyond_budget_is_a_regression()
    {
        var current = Fixtures.Report([Fixtures.Run(1, 1.2), Fixtures.Run(2, 1.2), Fixtures.Run(3, 1.2)]);

        var cmp = BaselineComparer.Compare(Baseline, current);

        var cold = cmp.Metrics.Single(m => m.Key == "query.logs.traceId.coldMs");
        Assert.Equal(Verdict.Regression, cold.Verdict);
        Assert.Equal(20, cold.WorsePercent!.Value, 6);
        // Throughput is higher-is-better: dividing it by 1.2 is also worse.
        var throughput = cmp.Metrics.Single(m => m.Key == "seed.itemsPerSecond");
        Assert.Equal(Verdict.Regression, throughput.Verdict);
        Assert.True(cmp.Regressions > 0);
        Assert.False(cmp.Passed);
        Assert.Equal(BaselineComparer.ExitFailed, BaselineComparer.ExitCode(cmp));
    }

    [Fact]
    public void Faster_beyond_budget_is_an_improvement_not_a_regression()
    {
        var current = Fixtures.Report([Fixtures.Run(1, 0.7), Fixtures.Run(2, 0.7), Fixtures.Run(3, 0.7)]);

        var cmp = BaselineComparer.Compare(Baseline, current);

        Assert.Equal(Verdict.Improvement, cmp.Metrics.Single(m => m.Key == "query.logs.traceId.coldMs").Verdict);
        Assert.Equal(Verdict.Improvement, cmp.Metrics.Single(m => m.Key == "seed.itemsPerSecond").Verdict);
        Assert.True(cmp.Passed);
    }

    [Fact]
    public void Noise_within_the_baselines_spread_passes()
    {
        var current = Fixtures.Report([Fixtures.Run(1, 1.03), Fixtures.Run(2, 1.04), Fixtures.Run(3, 1.02)]);

        var cmp = BaselineComparer.Compare(Baseline, current);

        Assert.Equal(0, cmp.Regressions);
        Assert.Empty(cmp.Incompatibilities);
        Assert.True(cmp.Passed);
        Assert.Equal(BaselineComparer.ExitPassed, BaselineComparer.ExitCode(cmp));
    }

    [Fact]
    public void Peak_rss_growth_is_caught()
    {
        var current = Fixtures.Report([Fixtures.Run(1, peakRss: 80 << 20), Fixtures.Run(2, peakRss: 80 << 20), Fixtures.Run(3, peakRss: 80 << 20)]);

        var cmp = BaselineComparer.Compare(Baseline, current);

        Assert.Equal(Verdict.Regression, cmp.Metrics.Single(m => m.Key == "memory.peakRssBytes").Verdict);
    }

    /// <summary>
    /// Review finding: a server that answers 429 quickly sends more responses per second while
    /// doing far less work. Attempted throughput rises; the comparison must still fail.
    /// </summary>
    [Fact]
    public void Collapsing_successful_throughput_behind_fast_429s_fails()
    {
        var healthy = Fixtures.Report(Enumerable.Range(1, 3).Select(i => Fixtures.Run(i, mixed: Fixtures.Mixed(accepted: 880, backpressure: 0))).ToList());
        // 220 → 450 responses/s, but 220 → 40 successful/s and 410/s turned away.
        var shedding = Fixtures.Report(Enumerable.Range(1, 3).Select(i => Fixtures.Run(i, mixed: Fixtures.Mixed(accepted: 160, backpressure: 1640))).ToList());

        var cmp = BaselineComparer.Compare(healthy, shedding);

        var metric = (string key) => cmp.Metrics.Single(m => m.Key == key);
        Assert.Equal(Verdict.NotGated, metric("mixed.requestsPerSecond").Verdict); // informational only
        Assert.True(metric("mixed.requestsPerSecond").CurrentMean > metric("mixed.requestsPerSecond").BaselineMean);
        Assert.Equal(Verdict.Regression, metric("mixed.acceptedPerSecond").Verdict);
        Assert.Equal(Verdict.Regression, metric("mixed.ingestEventsPerSecond").Verdict);
        var unsuccessful = metric("mixed.unsuccessfulPercent");
        Assert.Equal(Verdict.Regression, unsuccessful.Verdict);
        Assert.Equal(91.1, unsuccessful.WorsePercent!.Value, 1); // percentage points: 0% → 91.1%
        Assert.False(cmp.Passed);
        Assert.Equal(BaselineComparer.ExitFailed, BaselineComparer.ExitCode(cmp));
    }

    [Fact]
    public void A_small_rise_in_rejections_from_a_clean_baseline_is_caught()
    {
        var clean = Fixtures.Report(Enumerable.Range(1, 3).Select(i => Fixtures.Run(i, mixed: Fixtures.Mixed(880, 0))).ToList());
        var current = Fixtures.Report(Enumerable.Range(1, 3).Select(i => Fixtures.Run(i, mixed: Fixtures.Mixed(860, 20))).ToList());

        var unsuccessful = BaselineComparer.Compare(clean, current).Metrics.Single(m => m.Key == "mixed.unsuccessfulPercent");

        Assert.Equal(1, unsuccessful.BudgetPercent); // the 1-point floor: a 0% baseline is still gated
        Assert.Equal(Verdict.Regression, unsuccessful.Verdict); // 2.27 points
    }

    [Fact]
    public void A_missing_gated_metric_fails_the_comparison()
    {
        var current = Baseline with { Summary = Baseline.Summary.Where(m => m.Key != "memory.peakRssBytes").ToList() };

        var cmp = BaselineComparer.Compare(Baseline, current);

        Assert.Equal(Verdict.Missing, cmp.Metrics.Single(m => m.Key == "memory.peakRssBytes").Verdict);
        Assert.Equal(0, cmp.Regressions);
        Assert.Equal(1, cmp.MissingGated);
        Assert.False(cmp.Passed);
        Assert.Equal(BaselineComparer.ExitFailed, BaselineComparer.ExitCode(cmp));
        Assert.Contains(cmp.FailureReasons, r => r.Contains("missing"));
    }

    [Fact]
    public void A_report_missing_every_metric_fails()
    {
        var cmp = BaselineComparer.Compare(Baseline, Baseline with { Summary = [] });

        Assert.False(cmp.Passed);
        Assert.Contains(cmp.FailureReasons, r => r.Contains("nothing was actually compared"));
        Assert.Equal(BaselineComparer.ExitFailed, BaselineComparer.ExitCode(cmp));
    }

    [Fact]
    public void A_missing_ungated_metric_does_not_fail()
    {
        var ungated = Baseline.Summary.First(m => m.BudgetPercent is null);
        var current = Baseline with { Summary = Baseline.Summary.Where(m => m.Key != ungated.Key).ToList() };

        var cmp = BaselineComparer.Compare(Baseline, current);

        Assert.Equal(0, cmp.MissingGated);
        Assert.True(cmp.Passed);
    }

    [Fact]
    public void A_short_baseline_gates_nothing_and_cannot_pass()
    {
        var shortBaseline = Fixtures.Report([Fixtures.Run(1), Fixtures.Run(2)]);
        var current = Fixtures.Report([Fixtures.Run(1, 2), Fixtures.Run(2, 2)]);

        var cmp = BaselineComparer.Compare(shortBaseline, current);

        Assert.Equal(0, cmp.Regressions);
        Assert.All(cmp.Metrics, m => Assert.Equal(Verdict.NotGated, m.Verdict));
        Assert.False(cmp.Passed);
        Assert.Contains(cmp.FailureReasons, r => r.Contains("no metric was gated"));
    }

    [Fact]
    public void Different_configuration_or_host_fails_the_comparison()
    {
        var current = Fixtures.Report([Fixtures.Run(1), Fixtures.Run(2), Fixtures.Run(3)], configSha: new string('f', 64), cpu: "Other CPU");

        var cmp = BaselineComparer.Compare(Baseline, current);

        Assert.Contains(cmp.Incompatibilities, w => w.StartsWith("pinned configuration"));
        Assert.Contains(cmp.Incompatibilities, w => w.StartsWith("CPU differs"));
        Assert.Equal(0, cmp.Regressions);
        Assert.False(cmp.Passed);
        Assert.Equal(BaselineComparer.ExitFailed, BaselineComparer.ExitCode(cmp));
    }

    [Fact]
    public void A_different_number_of_runs_is_not_a_workload_change()
    {
        var current = Fixtures.Report([Fixtures.Run(1), Fixtures.Run(2), Fixtures.Run(3), Fixtures.Run(4), Fixtures.Run(5)]);

        var cmp = BaselineComparer.Compare(Baseline, current);

        Assert.Empty(cmp.Incompatibilities);
        Assert.True(cmp.Passed);
    }

    [Fact]
    public void A_different_seed_is_a_workload_change_and_fails()
    {
        var current = Baseline with { Workload = Baseline.Workload with { Seed = 7 } };

        var cmp = BaselineComparer.Compare(Baseline, current);

        Assert.Contains(cmp.Incompatibilities, w => w.StartsWith("workload differs"));
        Assert.False(cmp.Passed);
    }
}
