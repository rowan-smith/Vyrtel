using Vyrtel.Harness;

namespace Vyrtel.Profiling;

/// <summary>Command-line options for the profiler.</summary>
internal sealed record ProfileCliOptions
{
    public string Url { get; init; } = "http://127.0.0.1:8080";
    public string? Server { get; init; }
    public string? DataDir { get; init; }
    public string LogLevel { get; init; } = "warn";
    public int Events { get; init; } = 200_000;
    public int Batch { get; init; } = 500;
    public double Duration { get; init; } = 30;
    public int Concurrency { get; init; } = 4;
    public string Output { get; init; } = DefaultOutput();
    public bool NoLaunch { get; init; }
    public bool KeepData { get; init; }
    public bool ShowHelp { get; init; }

    public static ProfileCliOptions Parse(string[] args)
    {
        var options = new ProfileCliOptions();
        for (var i = 0; i < args.Length; i++)
        {
            var arg = args[i];
            var next = () => i + 1 < args.Length ? args[++i] : throw new ArgumentException($"{arg} requires a value");
            options = arg switch
            {
                "--url" => options with { Url = next() },
                "--server" => options with { Server = next() },
                "--data-dir" => options with { DataDir = next() },
                "--log-level" => options with { LogLevel = next() },
                "--events" => options with { Events = Math.Max(0, int.Parse(next())) },
                "--batch" => options with { Batch = Math.Max(1, int.Parse(next())) },
                "--duration" => options with { Duration = Math.Max(0.1, double.Parse(next())) },
                "--concurrency" => options with { Concurrency = Math.Max(1, int.Parse(next())) },
                "--out" => options with { Output = next() },
                "--no-launch" => options with { NoLaunch = true },
                "--keep-data" => options with { KeepData = true },
                "-h" or "--help" => options with { ShowHelp = true },
                _ => throw new ArgumentException($"unknown argument '{arg}' (use --help)"),
            };
        }
        return options;
    }

    private static string DefaultOutput()
    {
        var root = VyrtelServer.FindRepositoryRoot() ?? Directory.GetCurrentDirectory();
        return Path.Combine(root, "target", "profiling", "report.json");
    }

    public static void PrintHelp()
    {
        Console.WriteLine(
            """
            Vyrtel latency profiler

            Usage: Vyrtel.Profiling [options]

              --url <url>         Base URL of the server (default http://127.0.0.1:8080)
              --server <path>     Path to the vyrtel binary to launch if none is running
              --data-dir <path>   Data directory for the launched server (default: temp dir)
              --log-level <lvl>   Server log level (default warn)
              --events <n>        Log events to seed before profiling (default 200000, 0 = skip)
              --batch <n>         Events per seeded request (default 500)
              --duration <secs>   How long to run the mixed load (default 30)
              --concurrency <n>   Concurrent workers (default 4)
              --out <path>        Where to write the report (default target/profiling/report.json)
              --no-launch         Require an already-running server instead of launching one
              --keep-data         Keep the temporary data directory after the run
              -h, --help          Show this help
            """);
    }
}