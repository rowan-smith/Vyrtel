namespace Vyrtel.Baseline.Tests;

/// <summary>AC3 probes (CPU, disk, peak RSS source) and the command line.</summary>
public class HostAndOptionsTests
{
    [Fact]
    public void Reads_the_x86_model_name()
    {
        const string cpuinfo = "processor\t: 0\nvendor_id\t: GenuineIntel\nmodel name\t: AMD EPYC 7763 64-Core Processor\nflags\t\t: fpu\n";

        Assert.Equal("AMD EPYC 7763 64-Core Processor", HostProbe.ParseCpuInfo(cpuinfo));
    }

    [Fact]
    public void Falls_back_to_implementer_and_part_on_arm()
    {
        const string cpuinfo = "processor\t: 0\nBogoMIPS\t: 50.00\nCPU implementer\t: 0x41\nCPU architecture: 8\nCPU part\t: 0xd0c\n\nprocessor\t: 1\nCPU implementer\t: 0x41\n";

        Assert.Equal("CPU implementer 0x41, part 0xd0c", HostProbe.ParseCpuInfo(cpuinfo));
    }

    [Fact]
    public void Unknown_cpuinfo_is_null() => Assert.Null(HostProbe.ParseCpuInfo("processor : 0\n"));

    [Fact]
    public void Describes_this_host()
    {
        var host = HostProbe.Describe(Path.GetTempPath(), "notes");

        Assert.True(host.LogicalCores > 0);
        Assert.True(host.TotalMemoryBytes > 0);
        Assert.False(string.IsNullOrWhiteSpace(host.CpuModel));
        Assert.NotNull(host.Disk);
        Assert.True(host.Disk.TotalBytes > 0);
        Assert.Equal("notes", host.Notes);
    }

    [Theory]
    [InlineData(60_000_000, 55_000_000, 60_000_000, RssMonitor.OsPeakSource)]   // OS saw a spike between samples
    [InlineData(55_000_000, 55_000_000, 55_000_000, RssMonitor.OsPeakSource)]
    [InlineData(0, 55_000_000, 55_000_000, RssMonitor.SampledSource)]           // macOS: no OS peak
    [InlineData(40_000_000, 55_000_000, 55_000_000, RssMonitor.SampledSource)]  // never under-report
    public void Peak_rss_takes_the_larger_trustworthy_value(long osPeak, long sampled, long expected, string source) =>
        Assert.Equal((expected, source), RssMonitor.Choose(osPeak, sampled));

    [Fact]
    public void Defaults_are_a_three_run_baseline()
    {
        var o = BaselineOptions.Parse([]);

        Assert.Equal(3, o.Runs);
        Assert.Equal(42, o.Seed);
        Assert.Equal(BaselineOptions.DefaultPort, o.Port);
        Assert.EndsWith(Path.Combine("target", "baseline", "report.json"), o.Output);
    }

    [Fact]
    public void Parses_every_option()
    {
        var o = BaselineOptions.Parse([
            "--runs", "5", "--seed", "7", "--events", "1000", "--batch", "100", "--warm-iterations", "9",
            "--duration", "12.5", "--warmup", "2", "--concurrency", "8", "--port", "19000", "--server", "bin/vyrtel",
            "--config", "x.toml", "--label", "v0.1.2", "--notes", "CI", "--out", "o.json", "--compare", "b.json",
            "--log-level", "info", "--keep-data",
        ]);

        Assert.Equal((5, 7, 1000, 100, 9), (o.Runs, o.Seed, o.Events, o.Batch, o.WarmIterations));
        Assert.Equal((12.5, 2.0, 8, 19000), (o.Duration, o.Warmup, o.Concurrency, o.Port));
        Assert.Equal(("bin/vyrtel", "x.toml", "v0.1.2", "CI", "o.json", "b.json", "info"),
            (o.Server, o.Config, o.Label, o.Notes, o.Output, o.CompareWith, o.LogLevel));
        Assert.True(o.KeepData);
    }

    [Fact]
    public void Compare_subcommand_takes_two_reports() =>
        Assert.Equal(("a.json", "b.json"), BaselineOptions.Parse(["compare", "a.json", "b.json"]).CompareOnly);

    [Theory]
    [InlineData("--runs", "0")]
    [InlineData("--bogus")]
    [InlineData("--duration", "5", "--warmup", "5")]
    [InlineData("compare", "only-one.json")]
    [InlineData("--seed")]
    public void Rejects_invalid_arguments(params string[] args) =>
        Assert.Throws<ArgumentException>(() => BaselineOptions.Parse(args));

    [Fact]
    public void Configuration_hash_ignores_line_endings()
    {
        var dir = Directory.CreateTempSubdirectory("vyrtel-baseline-hash-");
        try
        {
            var lf = Path.Combine(dir.FullName, "lf.toml");
            var crlf = Path.Combine(dir.FullName, "crlf.toml");
            File.WriteAllText(lf, "[storage]\nzstd_level = 3\n");
            File.WriteAllText(crlf, "[storage]\r\nzstd_level = 3\r\n");

            Assert.Equal(HostProbe.Sha256OfText(lf), HostProbe.Sha256OfText(crlf));
            File.WriteAllText(crlf, "[storage]\r\nzstd_level = 4\r\n");
            Assert.NotEqual(HostProbe.Sha256OfText(lf), HostProbe.Sha256OfText(crlf));
        }
        finally
        {
            dir.Delete(recursive: true);
        }
    }

    [Fact]
    public void Finds_the_checked_in_pinned_configuration()
    {
        var path = BaselineOptions.Parse([]).ResolveConfigPath();

        Assert.EndsWith("baseline.toml", path);
        var text = File.ReadAllText(path);
        Assert.Contains("compaction = false", text);
        Assert.Contains("ingest_api_key = false", text);
    }
}
