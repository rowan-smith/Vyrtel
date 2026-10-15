using System.Globalization;
using Vyrtel.Harness;

namespace Vyrtel.Baseline;

/// <summary>Command-line options for a baseline run, or for comparing two existing reports.</summary>
public sealed record BaselineOptions
{
    public const int DefaultPort = 18480;

    public int Runs { get; init; } = 3;
    public int Seed { get; init; } = 42;
    public int Events { get; init; } = 100_000;
    public int Batch { get; init; } = 500;
    public int WarmIterations { get; init; } = 20;
    public double Duration { get; init; } = 20;
    public double Warmup { get; init; } = 3;
    public int Concurrency { get; init; } = 4;
    public int Port { get; init; } = DefaultPort;
    public string? Server { get; init; }
    public string? Config { get; init; }
    public string Label { get; init; } = "";
    public string Notes { get; init; } = "";
    public string LogLevel { get; init; } = "warn";
    public string Output { get; init; } = DefaultOutput();
    public string? CompareWith { get; init; }
    public bool KeepData { get; init; }
    public bool ShowHelp { get; init; }

    /// <summary><c>compare &lt;baseline.json&gt; &lt;current.json&gt;</c>: compare two reports without running.</summary>
    public (string Baseline, string Current)? CompareOnly { get; init; }

    public static BaselineOptions Parse(string[] args)
    {
        var options = new BaselineOptions();
        if (args.Length > 0 && args[0] == "compare")
        {
            if (args.Length != 3)
            {
                throw new ArgumentException("usage: compare <baseline.json> <current.json>");
            }
            return options with { CompareOnly = (args[1], args[2]) };
        }

        for (var i = 0; i < args.Length; i++)
        {
            var arg = args[i];
            var next = () => i + 1 < args.Length ? args[++i] : throw new ArgumentException($"{arg} requires a value");
            options = arg switch
            {
                "--runs" => options with { Runs = Positive(arg, Int(next())) },
                "--seed" => options with { Seed = Int(next()) },
                "--events" => options with { Events = Positive(arg, Int(next())) },
                "--batch" => options with { Batch = Positive(arg, Int(next())) },
                "--warm-iterations" => options with { WarmIterations = Positive(arg, Int(next())) },
                "--duration" => options with { Duration = Math.Max(1, Double(next())) },
                "--warmup" => options with { Warmup = Math.Max(0, Double(next())) },
                "--concurrency" => options with { Concurrency = Positive(arg, Int(next())) },
                "--port" => options with { Port = Positive(arg, Int(next())) },
                "--server" => options with { Server = next() },
                "--config" => options with { Config = next() },
                "--label" => options with { Label = next() },
                "--notes" => options with { Notes = next() },
                "--log-level" => options with { LogLevel = next() },
                "--out" => options with { Output = next() },
                "--compare" => options with { CompareWith = next() },
                "--keep-data" => options with { KeepData = true },
                "-h" or "--help" => options with { ShowHelp = true },
                _ => throw new ArgumentException($"unknown argument '{arg}' (use --help)"),
            };
        }
        if (options.Warmup >= options.Duration)
        {
            throw new ArgumentException("--warmup must be shorter than --duration");
        }
        return options;
    }

    /// <summary>The pinned configuration: <c>--config</c>, else <c>baseline.toml</c> beside the tool or in the repository.</summary>
    public string ResolveConfigPath()
    {
        if (Config is not null)
        {
            return Path.GetFullPath(Config);
        }
        var candidates = new List<string> { Path.Combine(AppContext.BaseDirectory, "baseline.toml") };
        if (VyrtelServer.FindRepositoryRoot() is { } root)
        {
            candidates.Insert(0, Path.Combine(root, "dotnet", "Vyrtel.Baseline", "baseline.toml"));
        }
        return candidates.FirstOrDefault(File.Exists)
            ?? throw new FileNotFoundException("Pinned configuration baseline.toml not found; pass --config <file>.");
    }

    private static int Int(string s) => int.Parse(s, CultureInfo.InvariantCulture);
    private static double Double(string s) => double.Parse(s, CultureInfo.InvariantCulture);

    private static int Positive(string arg, int value) =>
        value > 0 ? value : throw new ArgumentException($"{arg} must be at least 1");

    private static string DefaultOutput()
    {
        var root = VyrtelServer.FindRepositoryRoot() ?? Directory.GetCurrentDirectory();
        return Path.Combine(root, "target", "baseline", "report.json");
    }

    public static void PrintHelp()
    {
        Console.WriteLine(
            """
            Vyrtel performance baseline

            Usage: Vyrtel.Baseline [options]
                   Vyrtel.Baseline compare <baseline.json> <current.json>

            Each run launches a release server with the pinned configuration on a fresh data
            directory, seeds the deterministic dataset, measures every query cold (first request
            after a restart) and warm, then runs a concurrent ingest + query mix. Reports are
            written as JSON, Markdown and a CSV of raw samples.

              --runs <n>             Independent runs; 3 or more propose regression budgets (default 3)
              --seed <n>             Dataset and workload seed (default 42)
              --events <n>           Log events to seed per run (default 100000)
              --batch <n>            Events per seeding request (default 500)
              --warm-iterations <n>  Warm repetitions of each query after its cold request (default 20)
              --duration <secs>      Mixed ingest + query load per run (default 20)
              --warmup <secs>        Discarded start of the mixed load (default 3)
              --concurrency <n>      Mixed-load workers (default 4)
              --port <n>             Port for the launched server; must be free (default 18480)
              --server <path>        vyrtel binary (default: VYRTEL_BIN, then target/release)
              --config <path>        Pinned server configuration (default dotnet/Vyrtel.Baseline/baseline.toml)
              --label <text>         Name for this baseline, e.g. a tag or branch
              --notes <text>         Host facts the tool cannot detect, e.g. "NVMe SSD, AC power"
              --out <path>           Report path (default target/baseline/report.json)
              --compare <path>       Compare this run against an earlier report
              --log-level <lvl>      Server log level (default warn)
              --keep-data            Keep each run's data directory
              -h, --help             Show this help

            Exit status: 0 success, 1 error, 2 the comparison did not pass (a regression, a
            missing gated metric, nothing gated, or incompatible reports).
            """);
    }
}
