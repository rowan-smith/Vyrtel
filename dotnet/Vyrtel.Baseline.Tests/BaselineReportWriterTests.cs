namespace Vyrtel.Baseline.Tests;

/// <summary>AC1–AC3: what a reviewer reads in the report, and that the raw data survives.</summary>
public sealed class BaselineReportWriterTests : IDisposable
{
    private readonly string _dir = Path.Combine(Path.GetTempPath(), "vyrtel-baseline-tests-" + Guid.NewGuid().ToString("N"));
    private readonly BaselineReport _report = Fixtures.Report([Fixtures.Run(1, 1.0), Fixtures.Run(2, 1.01), Fixtures.Run(3, 0.99)]);

    public void Dispose()
    {
        if (Directory.Exists(_dir)) Directory.Delete(_dir, recursive: true);
    }

    [Fact]
    public void States_hardware_duration_peak_rss_binary_size_and_compression()
    {
        var md = BaselineReportWriter.ToMarkdown(_report);

        Assert.Contains("| CPU | Test CPU 9000, 22 logical cores |", md);
        Assert.Contains("| Memory | 32.0 GB", md);
        Assert.Contains("| Disk | C:\\ NTFS (Fixed)", md);
        Assert.Contains("| Notes | NVMe SSD, AC power |", md);
        Assert.Contains("- Duration: 2.3 min for 3 run(s) (45 s, 45 s, 45 s)", md);
        Assert.Contains("| 1 | 50.0 MB | 12.0 MB | 36.0 MB | 20.0 MB | 50.0 MB | os-peak |", md);
        Assert.Contains("| Binary | 11.8 MB (release)", md);
        Assert.Contains("| Compression | zstd level 3, independently compressed column chunks: 4.00× ratio, 7.6% index overhead", md);
        Assert.Contains("SHA-256 `3593a029c80e2d51…`", md);
        Assert.Contains("| Ingest admission | 3 concurrent (explicit)", md);
    }

    [Fact]
    public void States_accepted_and_rejected_counts_and_percentiles()
    {
        var md = BaselineReportWriter.ToMarkdown(_report);

        // seeding: 241 accepted + 2 × 429; mixed: 345 accepted + 5 × 429
        Assert.Contains("| 1 | seeding | 243 | 241 | 2 | 0 | 0 | 0 | 20,650 |", md);
        Assert.Contains("| 1 | mixed | 350 | 345 | 5 | 0 | 0 | 0 | 76,000 |", md);
        Assert.Contains("| ingest NDJSON (800 events) | 100 | 15 |", md); // 5 rejected per run × 3
    }

    [Fact]
    public void States_cold_and_warm_timings_with_diagnostics()
    {
        var md = BaselineReportWriter.ToMarkdown(_report);

        Assert.Contains("| traceId = … (Bloom filter) | selective | 1.60 ms ±1% |", md);
        Assert.Contains("| message contains \"timed out\" (scan) | scan |", md);
        // segments skipped/considered, blocks read (skipped), examined → matched, bytes cold → warm, indexes
        Assert.Contains("| 3/4 | 2 (19) | 2,404 → 1 | 4 KB → 4 KB | timestamp:time, traceId:bloom |", md);
    }

    [Fact]
    public void States_budgets_with_their_justification()
    {
        var md = BaselineReportWriter.ToMarkdown(_report);

        Assert.Contains("## Run-to-run variance and proposed regression budgets", md);
        Assert.Contains("3 runs, CV 1.0% → 90% upper bound 3.1% (×3.08) → 3× = 9.2%, rounded up to 10%; floor 15% → 15%", md);
        Assert.Contains("| Binary size | lower | 3 | 11.81 MB | 0.0 B | 0.0% |", md);
    }

    [Fact]
    public void Includes_the_comparison_when_there_is_one()
    {
        var current = Fixtures.Report([Fixtures.Run(1, 1.5), Fixtures.Run(2, 1.5), Fixtures.Run(3, 1.5)]);
        var withComparison = current with { Comparison = BaselineComparer.Compare(_report, current) };

        var md = BaselineReportWriter.ToMarkdown(withComparison);

        Assert.Contains("## Comparison with test (0.1.1, 2026-10-15)", md);
        Assert.Contains("**Failed**:", md);
        Assert.Contains("metric(s) regressed beyond their budget", md);
        Assert.Contains("| **regression** |", md);
    }

    [Fact]
    public void A_passing_comparison_says_how_much_it_gated()
    {
        var current = Fixtures.Report([Fixtures.Run(1, 1.02), Fixtures.Run(2, 1.02), Fixtures.Run(3, 1.02)]);
        var cmp = BaselineComparer.Compare(_report, current);

        var md = BaselineReportWriter.ToMarkdown(current with { Comparison = cmp });

        Assert.Contains($"**Passed**: {cmp.Gated} gated metric(s), none regressed.", md);
        // Ratios are reported in percentage points, not relative percent.
        Assert.Contains("| Mixed load requests not accepted | 1.43% | 1.43% | 0.00 pt | 1 pt | within budget |", md);
    }

    [Fact]
    public void Json_round_trips_and_samples_go_to_csv()
    {
        var paths = BaselineReportWriter.Write(_report, Path.Combine(_dir, "report.json"));

        Assert.Equal(3, paths.Count);
        var read = BaselineReportWriter.Read(paths[0]);
        Assert.Equal(_report.Summary.Select(m => (m.Key, m.Mean, m.BudgetPercent)), read.Summary.Select(m => (m.Key, m.Mean, m.BudgetPercent)));
        Assert.Equal(_report.Runs[0].Mixed.Requests.RejectedBackpressure, read.Runs[0].Mixed.Requests.RejectedBackpressure);
        Assert.Equal(_report.Runs[0].Queries[0].ColdDiagnostics, read.Runs[0].Queries[0].ColdDiagnostics! with { Indexes = _report.Runs[0].Queries[0].ColdDiagnostics!.Indexes });
        Assert.DoesNotContain("\"samples\"", File.ReadAllText(paths[0]));
        Assert.Contains("\"better\": \"lower\"", File.ReadAllText(paths[0]));

        var csv = File.ReadAllLines(paths[2]);
        Assert.Equal("run,phase,operation,latency_ms,status", csv[0]);
        Assert.Equal(1 + 2 * 3, csv.Length);
        Assert.Contains("1,mixed,ingest.ndjson,20,429", csv);
    }

    [Fact]
    public void Refuses_a_report_with_another_schema()
    {
        Directory.CreateDirectory(_dir);
        var path = Path.Combine(_dir, "old.json");
        var json = System.Text.Json.JsonSerializer.Serialize(_report with { SchemaVersion = 99 }, BaselineReportWriter.JsonOptions);
        File.WriteAllText(path, json);

        var ex = Assert.Throws<InvalidDataException>(() => BaselineReportWriter.Read(path));
        Assert.Contains("schema 99", ex.Message);
    }
}
