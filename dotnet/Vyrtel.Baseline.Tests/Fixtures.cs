using Vyrtel.Harness;

namespace Vyrtel.Baseline.Tests;

/// <summary>Small, fully specified reports so tests state their inputs instead of measuring.</summary>
internal static class Fixtures
{
    public static LatencyStats Latency(params double[] samples)
    {
        var sorted = samples.ToArray();
        Array.Sort(sorted);
        return LatencyStats.From(sorted);
    }

    public static RequestCounts Counts(params (int Status, long Count)[] statuses) =>
        RequestTally.Summarize(statuses.Select(s => new KeyValuePair<int, long>(s.Status, s.Count)));

    public static QueryDiagnostics Diagnostics(long bytesRead = 4096) =>
        new(1.5, 4, 3, 21, 19, 2, 2404, 1, bytesRead, 0, ["timestamp:time", "traceId:bloom"]);

    /// <summary>A run whose timings are all scaled by <paramref name="scale"/>.</summary>
    public static RunResult Run(int run, double scale = 1, long peakRss = 50 << 20)
    {
        var queries = new List<QueryMeasurement>
        {
            new("logs.traceId", "traceId = … (Bloom filter)", "selective", 1.6 * scale, Diagnostics(), Latency(0.4 * scale, 0.5 * scale, 0.6 * scale), Diagnostics(), 510, 0),
            new("logs.scan.timedOut", "message contains \"timed out\" (scan)", "scan", 11 * scale, Diagnostics(1 << 20), Latency(10 * scale, 11 * scale), Diagnostics(1 << 20), 515, 0),
        };
        var mixedOps = new List<MixedOperationResult>
        {
            new("ingest.ndjson", "ingest NDJSON (800 events)", Counts((200, 95), (429, 5)), Latency(4 * scale, 5 * scale, 20 * scale)),
            new("logs.latest", "latest logs (limit 200)", Counts((200, 250)), Latency(9 * scale, 10 * scale, 17 * scale)),
        };
        return new RunResult(
            run,
            45,
            new IngestPhase(0.5, 20_250, 20_250, Counts((200, 241), (429, 2)), 40_000 / scale, Latency(0.3 * scale, 4 * scale, 9 * scale)),
            queries,
            new MixedPhase(4, 4, 396 / scale, 76_000, RequestTally.Sum(mixedOps.Select(o => o.Requests)), mixedOps),
            new MemoryFacts(peakRss, RssMonitor.OsPeakSource, 12 << 20, 36 << 20, 20 << 20, peakRss),
            new StorageProfile(8_000_000, 1_980_000, 20_250, 4, 4.0, 0.076, 9_000_000),
            2_000_000,
            512)
        {
            Samples = [new RawSample(run, "query.cold", "logs.traceId", 1.6 * scale, 200), new RawSample(run, "mixed", "ingest.ndjson", 20, 429)],
        };
    }

    public static BaselineReport Report(IReadOnlyList<RunResult> runs, string configSha = "3593a029c80e2d51aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", string cpu = "Test CPU 9000")
    {
        const long binary = 12_385_280;
        return new BaselineReport(
            BaselineReport.CurrentSchemaVersion,
            new DateTimeOffset(2026, 10, 15, 11, 0, 0, TimeSpan.Zero),
            "test",
            "0.1.1",
            new WorkloadSpec(42, 20_000, 200, 50, 500, runs.Count, 5, 5, 1, 4),
            new PinnedConfig("dotnet/Vyrtel.Baseline/baseline.toml", configSha, null, new IngestLimits(3, "explicit", 1 << 20)),
            new HostInfo("Test OS 1.0", "X64", cpu, 22, 32L << 30, new DiskInfo("C:\\", "NTFS", "Fixed", 500L << 30, 23L << 30), ".NET 10.0.8", "NVMe SSD, AC power"),
            new BuildFacts("release", binary, new string('c', 64), "b61cd28761aa0000", false, "rustc 1.96.1"),
            new CompressionInfo("zstd level 3, independently compressed column chunks", 3, 4.0, 0.076, 97.8, 4, 2_000_000),
            135,
            runs,
            BaselineStatistics.Summarize(runs, binary));
    }
}
