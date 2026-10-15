using System.Text.Json;
using System.Text.Json.Serialization;
using Vyrtel.Harness;

namespace Vyrtel.Baseline;

/// <summary>
/// One baseline: what ran (workload, pinned configuration, host, build), the raw per-run
/// measurements, and the run-to-run summary with proposed regression budgets.
/// </summary>
public sealed record BaselineReport(
    int SchemaVersion,
    DateTimeOffset GeneratedAt,
    string Label,
    string VyrtelVersion,
    WorkloadSpec Workload,
    PinnedConfig Config,
    HostInfo Host,
    BuildFacts Build,
    CompressionInfo? Compression,
    double TotalDurationSeconds,
    IReadOnlyList<RunResult> Runs,
    IReadOnlyList<MetricSummary> Summary,
    ComparisonResult? Comparison = null)
{
    // 2: successful-throughput and failure-share metrics, dataset counts, pass/fail comparisons.
    // A schema-1 baseline gated attempted throughput (429s included), so it is refused, not reinterpreted.
    public const int CurrentSchemaVersion = 2;
}

/// <summary>The fixed workload. Two reports are comparable only when these match.</summary>
public sealed record WorkloadSpec(
    int Seed,
    int Events,
    int Traces,
    int MetricPoints,
    int Batch,
    int Runs,
    int WarmIterations,
    double MixedDurationSeconds,
    double MixedWarmupSeconds,
    int MixedConcurrency);

/// <summary>The pinned configuration file and what the server reported it was running with.</summary>
public sealed record PinnedConfig(string File, string Sha256, JsonElement? Effective, IngestLimits? Ingest);

/// <summary>Ingest admission as reported by <c>GET /api/v1/system/info</c>.</summary>
public sealed record IngestLimits(long MaxConcurrent, string ConcurrencySource, long MaxRequestBytes);

public sealed record HostInfo(
    string Os,
    string Architecture,
    string CpuModel,
    int LogicalCores,
    long TotalMemoryBytes,
    DiskInfo? Disk,
    string Runtime,
    string Notes);

public sealed record DiskInfo(string Mount, string Format, string Type, long TotalBytes, long FreeBytes);

public sealed record BuildFacts(
    string Profile,
    long ExecutableBytes,
    string? ExecutableSha256,
    string? GitCommit,
    bool? GitDirty,
    string? Rustc);

/// <summary>How segments are compressed and what that achieved on the seeded dataset.</summary>
public sealed record CompressionInfo(
    string Method,
    int? ZstdLevel,
    double? Ratio,
    double? IndexOverhead,
    double BytesStoredPerEvent,
    long SegmentCount,
    long DataDirBytes);

public sealed record RunResult(
    int Run,
    double DurationSeconds,
    IngestPhase Seed,
    IReadOnlyList<QueryMeasurement> Queries,
    MixedPhase Mixed,
    MemoryFacts Memory,
    StorageProfile? Storage,
    long DataDirBytes,
    double MeanRestartMs)
{
    /// <summary>Every latency sample of the run, written to the CSV rather than the JSON.</summary>
    [JsonIgnore]
    public IReadOnlyList<RawSample> Samples { get; init; } = [];
}

/// <summary>One request as measured: phase, operation, latency and HTTP status (0 = transport error).</summary>
public sealed record RawSample(int Run, string Phase, string Operation, double LatencyMs, int Status);

/// <summary>
/// Seeding the fixed dataset over HTTP, serially. Items are log events, traces and metric
/// points; rejected requests are retried, so the dataset always arrives complete.
/// </summary>
public sealed record IngestPhase(
    double Seconds,
    long ItemsSent,
    long ItemsAccepted,
    RequestCounts Requests,
    double ItemsPerSecond,
    LatencyStats Latency)
{
    /// <summary>Events per signal in the fixture, counted from the payloads.</summary>
    public SignalCounts Expected { get; init; } = SignalCounts.Zero;

    /// <summary>What the server acknowledged (native ingest reports a count; OTLP is counted from accepted payloads).</summary>
    public SignalCounts Acknowledged { get; init; } = SignalCounts.Zero;

    /// <summary>What the server reported storing once its queues drained. A run only proceeds when all three match.</summary>
    public SignalCounts Stored { get; init; } = SignalCounts.Zero;
}

/// <summary>Requests by outcome. Backpressure is 429/503 (retryable); 413 is a request over the size limit.</summary>
public sealed record RequestCounts(
    long Total,
    long Accepted,
    long RejectedBackpressure,
    long RejectedTooLarge,
    long RejectedOther,
    long TransportErrors,
    IReadOnlyDictionary<string, long> ByStatus)
{
    public long Rejected => RejectedBackpressure + RejectedTooLarge + RejectedOther;
}

public sealed record QueryMeasurement(
    string Name,
    string Label,
    string Category,
    double ColdMs,
    QueryDiagnostics? ColdDiagnostics,
    LatencyStats Warm,
    QueryDiagnostics? WarmDiagnostics,
    double RestartMs,
    int Failures);

/// <summary>The server's own execution statistics for a query (the <c>diagnostics</c> object).</summary>
public sealed record QueryDiagnostics(
    double ElapsedMs,
    long SegmentsConsidered,
    long SegmentsSkipped,
    long BlocksConsidered,
    long BlocksSkipped,
    long BlocksRead,
    long EventsExamined,
    long EventsMatched,
    long BytesRead,
    long UnsealedEventsScanned,
    IReadOnlyList<string> Indexes);

public sealed record MixedPhase(
    double MeasuredSeconds,
    int Concurrency,
    double RequestsPerSecond,
    long EventsAccepted,
    RequestCounts Requests,
    IReadOnlyList<MixedOperationResult> Operations);

public sealed record MixedOperationResult(string Name, string Label, RequestCounts Requests, LatencyStats Latency);

/// <summary>Server memory. Peak RSS is the larger of the OS-tracked peak and 100 ms sampling.</summary>
public sealed record MemoryFacts(
    long PeakRssBytes,
    string PeakRssSource,
    long IdleRssBytes,
    long SeedPeakRssBytes,
    long QueryPeakRssBytes,
    long MixedPeakRssBytes);

public enum Better
{
    Lower,
    Higher,
}

public enum MetricKind
{
    /// <summary>Latency, throughput or memory: subject to scheduling and cache noise.</summary>
    Timing,

    /// <summary>On-disk sizes: close to deterministic for a fixed dataset and build.</summary>
    Size,

    /// <summary>A percentage of requests; budgets are absolute percentage points, not relative to the mean.</summary>
    Ratio,
}

/// <summary>A metric across runs, with its proposed regression budget.</summary>
public sealed record MetricSummary(
    string Key,
    string Label,
    string Unit,
    Better Better,
    MetricKind Kind,
    int Runs,
    double Mean,
    double StdDev,
    double? CvPercent,
    double Min,
    double Max,
    double? BudgetPercent,
    string Justification,
    IReadOnlyList<double> Values);

public enum Verdict
{
    WithinBudget,
    Regression,
    Improvement,
    NotGated,
    Missing,
}

public sealed record MetricComparison(
    string Key,
    string Label,
    string Unit,
    double BaselineMean,
    double? CurrentMean,
    double? WorsePercent,
    double? BudgetPercent,
    Verdict Verdict);

public sealed record ComparisonResult(
    string BaselineLabel,
    DateTimeOffset BaselineGeneratedAt,
    string BaselineVersion,
    IReadOnlyList<string> Incompatibilities,
    IReadOnlyList<MetricComparison> Metrics)
{
    public int Regressions => Metrics.Count(m => m.Verdict == Verdict.Regression);

    /// <summary>Metrics the baseline gated that the current report does not have.</summary>
    public int MissingGated => Metrics.Count(m => m.Verdict == Verdict.Missing && m.BudgetPercent is not null);

    /// <summary>Metrics actually judged against a budget.</summary>
    public int Gated => Metrics.Count(m => m.Verdict is Verdict.WithinBudget or Verdict.Regression or Verdict.Improvement);

    /// <summary>Why the comparison did not pass; empty when it passed.</summary>
    public IReadOnlyList<string> FailureReasons
    {
        get
        {
            var reasons = new List<string>();
            if (Regressions > 0) reasons.Add($"{Regressions} metric(s) regressed beyond their budget");
            if (MissingGated > 0) reasons.Add($"{MissingGated} gated metric(s) missing from the current report");
            if (Gated == 0) reasons.Add("no metric was gated, so nothing was actually compared");
            reasons.AddRange(Incompatibilities.Select(i => $"incompatible reports: {i}"));
            return reasons;
        }
    }

    public bool Passed => FailureReasons.Count == 0;
}
