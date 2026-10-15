namespace Vyrtel.Baseline.Tests;

/// <summary>AC4: budgets follow from measured run-to-run variance, with the arithmetic recorded.</summary>
public class BaselineStatisticsTests
{
    [Fact]
    public void Summarizes_mean_sample_standard_deviation_and_cv()
    {
        var m = BaselineStatistics.Summarize("k", "K", "ms", Better.Lower, MetricKind.Timing, [90, 100, 110]);

        Assert.Equal(3, m.Runs);
        Assert.Equal(100, m.Mean, 9);
        Assert.Equal(10, m.StdDev, 9); // sample (n − 1), not population
        Assert.Equal(10, m.CvPercent!.Value, 9);
        Assert.Equal(90, m.Min);
        Assert.Equal(110, m.Max);
        Assert.Equal([90, 100, 110], m.Values);
    }

    [Theory]
    [InlineData(3, 3.081)]
    [InlineData(5, 1.939)]
    [InlineData(10, 1.469)]
    [InlineData(31, 1.207)] // last tabulated quantile (30 degrees of freedom)
    [InlineData(41, 1.173)] // Wilson–Hilferty: exact χ²₀.₁₀(40) = 29.0505 → 1.1734
    public void Upper_bound_factor_shrinks_as_runs_grow(int runs, double expected) =>
        Assert.Equal(expected, BaselineStatistics.UpperBoundFactor(runs), 2);

    [Fact]
    public void Upper_bound_factor_needs_a_spread() =>
        Assert.Throws<ArgumentOutOfRangeException>(() => BaselineStatistics.UpperBoundFactor(1));

    [Fact]
    public void Budget_is_three_upper_bound_cvs_rounded_up_to_five_percent_for_timings()
    {
        var (budget, why) = BaselineStatistics.ProposeBudget(5, 100, 7.2, MetricKind.Timing);

        Assert.Equal(45, budget); // 7.2 × 1.94 = 14.0; × 3 = 41.9 → 45
        Assert.Contains("CV 7.2%", why);
        Assert.Contains("90% upper bound 14.0% (×1.94)", why);
        Assert.Contains("3× = 41.9%", why);
        Assert.EndsWith("→ 45%", why);
    }

    [Fact]
    public void The_same_spread_from_fewer_runs_gets_a_wider_budget()
    {
        var three = BaselineStatistics.ProposeBudget(3, 100, 4, MetricKind.Timing).Budget;
        var ten = BaselineStatistics.ProposeBudget(10, 100, 4, MetricKind.Timing).Budget;

        Assert.Equal(40, three); // 4 × 3.08 × 3 = 37.0
        Assert.Equal(20, ten);   // 4 × 1.47 × 3 = 17.6
    }

    /// <summary>
    /// Measured on the reference laptop: two baselines of the same commit, 3 runs each (latest
    /// logs, warm p95, ms). B's mean is 11.6% above A's. Taking A's 3-run CV (3.1%) at face value
    /// gave a 10% budget and a false regression; the upper bound gives 30%.
    /// </summary>
    [Fact]
    public void Same_commit_noise_measured_on_the_reference_laptop_is_not_a_regression()
    {
        var a = BaselineStatistics.Summarize("latest.p95", "latest p95", "ms", Better.Lower, MetricKind.Timing, [8.1627, 7.6892, 8.0382]);
        var b = BaselineStatistics.Summarize("latest.p95", "latest p95", "ms", Better.Lower, MetricKind.Timing, [8.6933, 8.3227, 9.6446]);

        var worse = (b.Mean - a.Mean) / a.Mean * 100;

        Assert.Equal(11.6, worse, 1);
        Assert.Equal(30, a.BudgetPercent);
        Assert.Equal(Verdict.WithinBudget, BaselineComparer.Judge(worse, a.BudgetPercent));
    }

    [Fact]
    public void Successful_throughput_counts_only_accepted_requests()
    {
        var run = Fixtures.Run(1, mixed: Fixtures.Mixed(accepted: 160, backpressure: 1640));

        var s = BaselineStatistics.Flatten(run, 1).ToDictionary(x => x.Key);

        Assert.Equal(450, s["mixed.requestsPerSecond"].Value);  // every response
        Assert.Equal(40, s["mixed.acceptedPerSecond"].Value);   // only 2xx
        Assert.Equal(8_800, s["mixed.ingestEventsPerSecond"].Value);
        Assert.Equal(91.11, s["mixed.unsuccessfulPercent"].Value, 2);
        Assert.Equal(Better.Lower, s["mixed.unsuccessfulPercent"].Better);
        Assert.Equal(MetricKind.Ratio, s["mixed.unsuccessfulPercent"].Kind);
    }

    [Fact]
    public void Attempted_throughput_is_informational_and_never_gated()
    {
        var summary = BaselineStatistics.Summarize([Fixtures.Run(1), Fixtures.Run(2), Fixtures.Run(3)], 1);

        var attempted = summary.Single(m => m.Key == "mixed.requestsPerSecond");
        Assert.Null(attempted.BudgetPercent);
        Assert.StartsWith("informational", attempted.Justification);
        Assert.NotNull(summary.Single(m => m.Key == "mixed.acceptedPerSecond").BudgetPercent);
    }

    [Theory]
    [InlineData(0.0, 1.0)] // no failures in any run: the 1-point floor
    [InlineData(0.5, 5.0)] // 0.5 × 3.08 × 3 = 4.6 → 5 points
    [InlineData(6.0, null)] // 55.5 points: too noisy
    public void Ratio_budgets_are_absolute_points(double stdDevPoints, double? expected) =>
        Assert.Equal(expected, BaselineStatistics.ProposeRatioBudget(3, stdDevPoints).Budget);

    [Fact]
    public void Ratio_budget_needs_three_runs() =>
        Assert.Null(BaselineStatistics.ProposeRatioBudget(2, 0).Budget);

    [Fact]
    public void Quiet_timings_get_the_timing_floor()
    {
        var (budget, why) = BaselineStatistics.ProposeBudget(3, 100, 1.0, MetricKind.Timing);

        Assert.Equal(BaselineStatistics.TimingFloorPercent, budget);
        Assert.Contains("floor 15%", why);
    }

    [Theory]
    [InlineData(0.50, 20)]
    [InlineData(0.95, 200)]
    [InlineData(0.99, 1000)]
    public void A_percentile_needs_ten_samples_beyond_it(double quantile, int expected) =>
        Assert.Equal(expected, BaselineStatistics.RequiredSamples(quantile));

    [Fact]
    public void A_tail_from_too_few_samples_is_not_gated()
    {
        // p99 of ~460 requests per run is decided by its top 4 samples.
        var m = BaselineStatistics.Summarize("k", "K", "ms", Better.Lower, MetricKind.Timing, [30.4, 30.5, 31.2], quantile: 0.99, fewestSamples: 460);

        Assert.Null(m.BudgetPercent);
        Assert.Equal("not gated: p99 from 460 samples in one run; at least 1000 are needed for 10 beyond it", m.Justification);
    }

    [Fact]
    public void A_percentile_with_enough_samples_is_gated()
    {
        var m = BaselineStatistics.Summarize("k", "K", "ms", Better.Lower, MetricKind.Timing, [18.1, 18.0, 18.2], quantile: 0.50, fewestSamples: 20);

        Assert.Equal(BaselineStatistics.TimingFloorPercent, m.BudgetPercent);
    }

    [Fact]
    public void The_fewest_samples_of_any_run_decides()
    {
        var samples = new[] { 1200, 1100, 999 }.Select((n, i) =>
            new MetricSample("p99", "p99", "ms", Better.Lower, MetricKind.Timing, 30 + i * 0.1, 0.99, n));

        var m = BaselineStatistics.Summarize(samples).Single();

        Assert.Null(m.BudgetPercent);
        Assert.Contains("from 999 samples", m.Justification);
    }

    [Theory]
    [InlineData(0.0, 2)] // identical binaries: the size floor
    [InlineData(1.2, 6)] // 1.2 × 1.47 × 3 = 5.3 → 6 (1% steps)
    public void Sizes_use_one_percent_steps_and_a_two_percent_floor(double cv, double expected) =>
        Assert.Equal(expected, BaselineStatistics.ProposeBudget(10, 1000, cv, MetricKind.Size).Budget);

    [Theory]
    [InlineData(1)]
    [InlineData(2)]
    public void Fewer_than_three_runs_are_not_gated(int runs)
    {
        var (budget, why) = BaselineStatistics.ProposeBudget(runs, 100, 1, MetricKind.Timing);

        Assert.Null(budget);
        Assert.Contains($"{runs} run(s)", why);
    }

    [Fact]
    public void A_zero_mean_is_not_gated()
    {
        var m = BaselineStatistics.Summarize("k", "K", "ms", Better.Lower, MetricKind.Timing, [0, 0, 0]);

        Assert.Null(m.CvPercent);
        Assert.Null(m.BudgetPercent);
        Assert.Contains("mean is zero", m.Justification);
    }

    [Fact]
    public void Too_noisy_metrics_are_not_gated_and_say_why()
    {
        var (budget, why) = BaselineStatistics.ProposeBudget(3, 100, 20, MetricKind.Timing);

        Assert.Null(budget); // 60% > 50%
        Assert.Contains("too noisy", why);
    }

    [Fact]
    public void Identical_runs_produce_the_floor_not_zero()
    {
        var m = BaselineStatistics.Summarize("k", "K", "bytes", Better.Lower, MetricKind.Size, [5, 5, 5]);

        Assert.Equal(0, m.CvPercent);
        Assert.Equal(BaselineStatistics.SizeFloorPercent, m.BudgetPercent);
    }

    [Fact]
    public void Every_run_contributes_cold_warm_mixed_memory_and_size_metrics()
    {
        var keys = BaselineStatistics.Flatten(Fixtures.Run(1), 12_000_000).Select(s => s.Key).ToHashSet();

        Assert.Contains("seed.itemsPerSecond", keys);
        Assert.Contains("query.logs.traceId.coldMs", keys);
        Assert.Contains("query.logs.traceId.warmP50Ms", keys);
        Assert.Contains("query.logs.scan.timedOut.warmP95Ms", keys);
        Assert.Contains("mixed.requestsPerSecond", keys);
        Assert.Contains("mixed.acceptedPerSecond", keys);
        Assert.Contains("mixed.ingestEventsPerSecond", keys);
        Assert.Contains("mixed.unsuccessfulPercent", keys);
        Assert.Contains("seed.unsuccessfulPercent", keys);
        Assert.Contains("mixed.ingest.ndjson.p95Ms", keys);
        Assert.Contains("mixed.ingest.ndjson.p99Ms", keys);
        Assert.Contains("memory.peakRssBytes", keys);
        Assert.Contains("storage.bytesPerEvent", keys);
        Assert.Contains("build.executableBytes", keys);
    }

    [Fact]
    public void Throughput_is_higher_is_better_and_latency_lower()
    {
        var samples = BaselineStatistics.Flatten(Fixtures.Run(1), 1).ToDictionary(s => s.Key);

        Assert.Equal(Better.Higher, samples["seed.itemsPerSecond"].Better);
        Assert.Equal(Better.Higher, samples["mixed.requestsPerSecond"].Better);
        Assert.Equal(Better.Lower, samples["query.logs.traceId.coldMs"].Better);
        Assert.Equal(MetricKind.Size, samples["build.executableBytes"].Kind);
        Assert.Equal((0.99, 3), (samples["mixed.ingest.ndjson.p99Ms"].Quantile, samples["mixed.ingest.ndjson.p99Ms"].Samples));
        Assert.Equal((0.50, 3), (samples["query.logs.traceId.warmP50Ms"].Quantile, samples["query.logs.traceId.warmP50Ms"].Samples));
        Assert.Null(samples["query.logs.traceId.coldMs"].Quantile);
    }

    [Fact]
    public void Summary_over_runs_has_one_entry_per_metric()
    {
        var runs = new[] { Fixtures.Run(1, 1.0), Fixtures.Run(2, 1.02), Fixtures.Run(3, 0.98) };

        var summary = BaselineStatistics.Summarize(runs, 12_000_000);

        Assert.Equal(summary.Count, summary.Select(m => m.Key).Distinct().Count());
        Assert.All(summary, m => Assert.Equal(3, m.Runs));
        var cold = summary.Single(m => m.Key == "query.logs.traceId.coldMs");
        Assert.Equal(1.6, cold.Mean, 6);
        Assert.Equal(20, cold.BudgetPercent); // CV 2% × 3.08 × 3 = 18.5 → 20
    }
}
