using Vyrtel.Harness;

namespace Vyrtel.Benchmark;

/// <summary>Command-line options for the benchmark runner.</summary>
internal sealed record CliOptions
{
    public string Url { get; init; } = "http://127.0.0.1:8080";
    public string? Server { get; init; }
    public string? DataDir { get; init; }
    public string LogLevel { get; init; } = "warn";
    public int Events { get; init; } = 200_000;
    public int Batch { get; init; } = 500;
    public string Job { get; init; } = "short";
    public string Output { get; init; } = DefaultOutput();
    public string Label { get; init; } = "";
    public bool NoLaunch { get; init; }
    public bool KeepData { get; init; }
    public bool List { get; init; }
    public bool ShowHelp { get; init; }

    public static CliOptions Parse(string[] args)
    {
        var options = new CliOptions();
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
                "--job" => options with { Job = next() },
                "--out" => options with { Output = next() },
                "--label" => options with { Label = next() },
                "--no-launch" => options with { NoLaunch = true },
                "--keep-data" => options with { KeepData = true },
                "--list" => options with { List = true },
                "-h" or "--help" => options with { ShowHelp = true },
                _ => throw new ArgumentException($"unknown argument '{arg}' (use --help)"),
            };
        }
        return options;
    }

    private static string DefaultOutput()
    {
        var root = VyrtelServer.FindRepositoryRoot() ?? Directory.GetCurrentDirectory();
        return Path.Combine(root, "docs", "benchmarks", "results.json");
    }

    public static void PrintHelp()
    {
        Console.WriteLine(
            """
            Vyrtel benchmark runner

            Usage: Vyrtel.Benchmark [options]

              --url <url>        Base URL of the server (default http://127.0.0.1:8080)
              --server <path>    Path to the vyrtel binary to launch if none is running
              --data-dir <path>  Data directory for the launched server (default: temp dir)
              --log-level <lvl>  Server log level (default warn)
              --events <n>       Log events to seed before benchmarking (default 200000, 0 = skip)
              --batch <n>        Events per seeded/batched request (default 500)
              --job <name>       BenchmarkDotNet job: dry, short, medium, default (default short)
              --out <path>       Where to write results.json (default docs/benchmarks/results.json)
              --label <name>     Version label for a version snapshot (default: server version)
              --no-launch        Require an already-running server instead of launching one
              --keep-data        Keep the temporary data directory after the run
              --list             List the benchmarks and exit
              -h, --help         Show this help
            """);
    }

    public static void PrintList()
    {
        foreach (var (method, meta) in BenchmarkCatalog.ByMethod)
        {
            Console.WriteLine($"{meta.Group,-6} {meta.Display} ({method})");
        }
    }
}
