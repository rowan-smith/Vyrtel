namespace Vyrtel.Harness;

/// <summary>
/// How the benchmark/profiler should reach a Vyrtel server. If a server is already
/// listening at <see cref="BaseUrl"/> it is used as-is; otherwise the harness launches
/// <see cref="ExecutablePath"/> against a throw-away data directory.
/// </summary>
public sealed record VyrtelServerOptions
{
    public Uri BaseUrl { get; init; } = new("http://127.0.0.1:8080");

    /// <summary>Path to the <c>vyrtel</c> binary. Falls back to the <c>VYRTEL_BIN</c> env var,
    /// then to <c>target/{release,debug}/vyrtel</c> in the repository.</summary>
    public string? ExecutablePath { get; init; }

    /// <summary>Data directory for a launched server (default: a new temp directory).</summary>
    public string? DataDir { get; init; }

    public string LogLevel { get; init; } = "warn";

    /// <summary>When false, fail if no server is already reachable instead of launching one.</summary>
    public bool AllowLaunch { get; init; } = true;

    public TimeSpan StartupTimeout { get; init; } = TimeSpan.FromSeconds(30);

    /// <summary>Keep the temporary data directory after the run (useful when debugging).</summary>
    public bool KeepData { get; init; }
}
