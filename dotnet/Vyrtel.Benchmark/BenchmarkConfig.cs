using BenchmarkDotNet.Configs;
using BenchmarkDotNet.Diagnosers;
using BenchmarkDotNet.Exporters;
using BenchmarkDotNet.Exporters.Json;
using BenchmarkDotNet.Jobs;
using BenchmarkDotNet.Loggers;

namespace Vyrtel.Benchmark;

public static class BenchmarkConfig
{
    /// <summary>
    /// Builds the BenchmarkDotNet configuration. The job name is exposed via the CLI so
    /// CI can be fast (<c>short</c>) while a local run can be thorough (<c>default</c>).
    /// </summary>
    public static IConfig Create(string job, string artifactsPath)
    {
        var selected = job.ToLowerInvariant() switch
        {
            "dry" => Job.Dry,
            "default" => Job.Default,
            "medium" => Job.MediumRun,
            _ => Job.ShortRun,
        };

        return ManualConfig.Create(DefaultConfig.Instance)
            .AddJob(selected.WithId(job.ToLowerInvariant()))
            .AddDiagnoser(MemoryDiagnoser.Default)
            .AddExporter(MarkdownExporter.Default, JsonExporter.Full)
            .AddLogger(ConsoleLogger.Default)
            .HideColumns("Error", "StdDev", "Gen0", "Gen1", "Gen2", "Allocated")
            .WithArtifactsPath(artifactsPath)
            .WithOptions(ConfigOptions.DisableOptimizationsValidator);
    }
}
