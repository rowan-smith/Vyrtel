using System.Diagnostics;

namespace Vyrtel.Harness;

/// <summary>
/// Connects to a running Vyrtel server, or launches one from a local executable and
/// tears it down on dispose. Used by both the benchmark and the profiler so they can
/// run unattended in CI.
/// </summary>
public sealed class VyrtelServer : IAsyncDisposable
{
    private readonly Process? _process;
    private readonly string? _dataDir;
    private readonly bool _keepData;

    public Uri BaseUrl { get; }
    public bool Launched => _process is not null;
    public string? ExecutablePath { get; }
    /// <summary>Size of the server executable on disk, in bytes (0 when connected to an external server).</summary>
    public long ExecutableSizeBytes { get; }

    private long? _footprintBytes;

    private VyrtelServer(Uri baseUrl, Process? process, string? dataDir, string? executable, bool keepData)
    {
        BaseUrl = baseUrl;
        _process = process;
        _dataDir = dataDir;
        ExecutablePath = executable;
        _keepData = keepData;
        ExecutableSizeBytes = executable is not null && File.Exists(executable)
            ? new FileInfo(executable).Length
            : 0;
    }

    /// <summary>
    /// Total size of the server's data directory on disk, in bytes. Measured on demand (and
    /// keep the last value once the directory has been removed), so the caller decides when —
    /// typically right after seeding.
    /// </summary>
    public long MeasureDataDirBytes()
    {
        if (_dataDir is null)
        {
            return 0;
        }
        _footprintBytes = Directory.Exists(_dataDir) ? DirectorySize(_dataDir) : _footprintBytes ?? 0;
        return _footprintBytes.Value;
    }

    /// <summary>
    /// The cargo profile the binary was built with (<c>release</c>/<c>debug</c>), or
    /// <c>external</c> when connected to a server the harness did not launch.
    /// </summary>
    public string BuildProfile => ProfileOf(ExecutablePath);

    /// <summary>
    /// Executable size plus the data directory footprint, sampled together so the report
    /// describes the same moment (the dataset right after seeding).
    /// </summary>
    public BuildInfo MeasureBuild() => new(
        Profile: BuildProfile,
        Executable: ExecutablePath,
        ExecutableBytes: ExecutableSizeBytes,
        DataDirBytes: MeasureDataDirBytes());

    internal static string ProfileOf(string? executable)
    {
        var path = executable?.Replace('\\', '/');
        if (path is null) return "external";
        if (path.Contains("/release/")) return "release";
        if (path.Contains("/debug/")) return "debug";
        return "external";
    }

    private static long DirectorySize(string path)
    {
        long total = 0;
        foreach (var file in new DirectoryInfo(path).EnumerateFiles("*", SearchOption.AllDirectories))
        {
            total += file.Length;
        }
        return total;
    }

    public static async Task<VyrtelServer> StartOrConnectAsync(VyrtelServerOptions options, CancellationToken ct = default)
    {
        if (await IsHealthyAsync(options.BaseUrl, ct))
        {
            Console.WriteLine($"vyrtel: using the server already listening at {options.BaseUrl}");
            return new VyrtelServer(options.BaseUrl, null, null, null, options.KeepData);
        }

        var executable = options.ExecutablePath
            ?? Environment.GetEnvironmentVariable("VYRTEL_BIN")
            ?? FindExecutable();

        if (!options.AllowLaunch)
        {
            throw new InvalidOperationException($"No Vyrtel server is listening at {options.BaseUrl} (launching is disabled).");
        }

        if (executable is null || !File.Exists(executable))
        {
            throw new InvalidOperationException(
                $"No Vyrtel server is listening at {options.BaseUrl} and no executable was found. " +
                "Build one with 'cargo build --release -p server', pass --server <path>, or set VYRTEL_BIN.");
        }

        var dataDir = options.DataDir
            ?? Path.Combine(Path.GetTempPath(), "vyrtel-harness-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(dataDir);

        var port = options.BaseUrl.Port > 0 ? options.BaseUrl.Port : 8080;
        var arguments = $"--data-dir \"{dataDir}\" --bind 127.0.0.1:{port} --log-level {options.LogLevel}";
        var startInfo = new ProcessStartInfo(executable, arguments)
        {
            UseShellExecute = false,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            CreateNoWindow = true,
        };

        var process = new Process { StartInfo = startInfo, EnableRaisingEvents = true };
        process.OutputDataReceived += (_, e) => { if (e.Data is not null) Console.WriteLine($"  [vyrtel] {e.Data}"); };
        process.ErrorDataReceived += (_, e) => { if (e.Data is not null) Console.WriteLine($"  [vyrtel] {e.Data}"); };

        Console.WriteLine($"vyrtel: launching {executable} (data-dir {dataDir})");
        process.Start();
        process.BeginOutputReadLine();
        process.BeginErrorReadLine();

        var deadline = DateTime.UtcNow + options.StartupTimeout;
        while (DateTime.UtcNow < deadline)
        {
            if (process.HasExited)
            {
                throw new InvalidOperationException(
                    $"Vyrtel exited with code {process.ExitCode} before becoming healthy.");
            }
            if (await IsHealthyAsync(options.BaseUrl, ct))
            {
                Console.WriteLine($"vyrtel: ready at {options.BaseUrl}");
                return new VyrtelServer(options.BaseUrl, process, dataDir, executable, options.KeepData);
            }
            await Task.Delay(200, ct);
        }

        try { process.Kill(entireProcessTree: true); } catch { /* best effort */ }
        throw new TimeoutException($"Vyrtel did not become healthy within {options.StartupTimeout}.");
    }

    private static async Task<bool> IsHealthyAsync(Uri baseUrl, CancellationToken ct)
    {
        try
        {
            using var http = new HttpClient { Timeout = TimeSpan.FromSeconds(2) };
            using var response = await http.GetAsync(new Uri(baseUrl, "health"), ct);
            return response.IsSuccessStatusCode;
        }
        catch
        {
            return false;
        }
    }

    /// <summary>Looks for a built binary under the repository's <c>target/</c> directory.</summary>
    public static string? FindExecutable()
    {
        var root = FindRepositoryRoot();
        if (root is null)
        {
            return null;
        }
        var name = OperatingSystem.IsWindows() ? "vyrtel.exe" : "vyrtel";
        foreach (var profile in new[] { "release", "debug" })
        {
            var candidate = Path.Combine(root, "target", profile, name);
            if (File.Exists(candidate))
            {
                return candidate;
            }
        }
        return null;
    }

    /// <summary>Walks up from the working directory and the assembly location to the Cargo workspace root.</summary>
    public static string? FindRepositoryRoot()
    {
        foreach (var start in new[] { Directory.GetCurrentDirectory(), AppContext.BaseDirectory })
        {
            var dir = new DirectoryInfo(start);
            while (dir is not null)
            {
                var cargo = Path.Combine(dir.FullName, "Cargo.toml");
                try
                {
                    if (File.Exists(cargo) && File.ReadAllText(cargo).Contains("[workspace]"))
                    {
                        return dir.FullName;
                    }
                }
                catch
                {
                    // ignore unreadable paths and keep walking up
                }
                dir = dir.Parent;
            }
        }
        return null;
    }

    public async ValueTask DisposeAsync()
    {
        if (_process is not null && !_process.HasExited)
        {
            Console.WriteLine("vyrtel: shutting down the server we launched");
            try
            {
                _process.Kill(entireProcessTree: true);
                await _process.WaitForExitAsync();
            }
            catch
            {
                // already gone
            }
        }
        _process?.Dispose();

        if (_dataDir is not null && !_keepData)
        {
            try
            {
                if (Directory.Exists(_dataDir))
                {
                    // keep the last footprint so the report can still read it
                    _footprintBytes ??= DirectorySize(_dataDir);
                    Directory.Delete(_dataDir, recursive: true);
                }
            }
            catch
            {
                // a lingering handle shouldn't fail the run
            }
        }
    }
}
