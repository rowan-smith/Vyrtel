namespace Vyrtel.Harness;

/// <summary>
/// On-disk footprint of the running server, as reported by <c>GET /api/v1/system/storage</c>.
/// Recorded once right after seeding, before the benchmark run, so the numbers describe the
/// dataset rather than the traffic the benchmarks generate.
/// </summary>
public sealed record StorageProfile(
    long RawBytes,
    long StoredBytes,
    long EventCount,
    long SegmentCount,
    double? CompressionRatio,
    double? IndexOverhead,
    long ReceivedBytes,
    long SegmentBytes = 0,
    long IndexBytes = 0,
    long WalBytes = 0,
    long MetadataBytes = 0)
{
    /// <summary>Compressed bytes stored per event — the headline storage-efficiency metric.</summary>
    public double BytesStoredPerEvent => EventCount > 0 ? (double)StoredBytes / EventCount : 0;
}

/// <summary>What was built and measured.</summary>
public sealed record BuildInfo(
    string Profile,
    [property: System.Text.Json.Serialization.JsonIgnore] string? Executable,
    long ExecutableBytes,
    long DataDirBytes)
{
    /// <summary>Executable size for reports, e.g. "11.4 MB".</summary>
    public string ExecutableSize => ByteFormat.Format(ExecutableBytes);

    /// <summary>Data-directory size for reports, e.g. "8.9 MB".</summary>
    public string DataDirSize => ByteFormat.Format(DataDirBytes);
}
