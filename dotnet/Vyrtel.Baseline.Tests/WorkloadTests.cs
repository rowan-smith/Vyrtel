using System.Text;
using System.Text.Json.Nodes;
using Vyrtel.Harness;

namespace Vyrtel.Baseline.Tests;

/// <summary>TC-02 and AC2: the workload repeats exactly, and covers selective queries and scans.</summary>
public class WorkloadTests
{
    private static readonly WorkloadContext Context = new("trace-a", "trace-b", EventFactory.DefaultMetricName);

    [Fact]
    public void Each_worker_issues_the_same_sequence_every_run()
    {
        var ops = Workload.Mixed(Context, seed: 42);

        var first = Workload.Sequence(ops, 42, worker: 0).Take(500).Select(o => o.Name).ToList();
        var again = Workload.Sequence(ops, 42, worker: 0).Take(500).Select(o => o.Name).ToList();
        var other = Workload.Sequence(ops, 42, worker: 1).Take(500).Select(o => o.Name).ToList();

        Assert.Equal(first, again);
        Assert.NotEqual(first, other);
    }

    [Fact]
    public void The_sequence_is_fixed_across_processes()
    {
        // Pinned so a change to the picking logic (or a per-process random seed) shows up here.
        var ops = Workload.Mixed(Context, seed: 42);

        var names = Workload.Sequence(ops, 42, worker: 0).Take(6).Select(o => o.Name);

        Assert.Equal(ExpectedFirstSix, string.Join(",", names));
    }

    // Recorded from the implementation; System.Random with an explicit seed is stable across .NET versions.
    private const string ExpectedFirstSix = "traces.search,logs.service,logs.level,logs.level,traces.get,ingest.json";

    [Fact]
    public void The_mix_follows_its_weights()
    {
        var ops = Workload.Mixed(Context, seed: 42);
        var counts = Workload.Sequence(ops, 42, 0).Take(124_000).GroupBy(o => o.Name).ToDictionary(g => g.Key, g => g.Count());

        // Weight 18 of 124 → ~18,000; weight 5 → ~5,000.
        Assert.InRange(counts["logs.latest"], 17_000, 19_000);
        Assert.InRange(counts["logs.scan"], 4_500, 5_500);
        Assert.Equal(ops.Count, counts.Count);
    }

    [Fact]
    public void Seeded_events_are_identical_apart_from_timestamps()
    {
        static List<string> Normalised(byte[] ndjson) =>
            Encoding.UTF8.GetString(ndjson).Split('\n', StringSplitOptions.RemoveEmptyEntries)
                .Select(line =>
                {
                    var node = JsonNode.Parse(line)!.AsObject();
                    node.Remove("timestamp");
                    return node.ToJsonString();
                })
                .ToList();

        var a = Normalised(new EventFactory(seed: 42).Ndjson(500));
        var b = Normalised(new EventFactory(seed: 42).Ndjson(500));

        Assert.Equal(a, b);
    }

    [Fact]
    public void Queries_cover_selective_lookups_and_message_scans()
    {
        var queries = Workload.Queries(Context);

        Assert.Contains(queries, q => q.Category == "selective" && q.Name == "logs.traceId");
        Assert.Contains(queries, q => q.Category == "selective" && q.Name == "logs.level");
        Assert.Contains(queries, q => q.Category == "selective" && q.Name == "logs.range");
        Assert.Equal(2, queries.Count(q => q.Category == "scan"));
        Assert.Equal(queries.Count, queries.Select(q => q.Name).Distinct().Count());
    }

    [Fact]
    public void Reads_the_servers_diagnostics()
    {
        const string body = """
            {"events":[],"diagnostics":{"elapsedMs":1.25,"segmentsConsidered":4,"segmentsSkipped":3,
             "segmentsSkippedByTime":0,"segmentsSkippedByIndex":3,"segmentsNotNeeded":0,"blocksConsidered":21,
             "blocksSkipped":19,"blocksRead":2,"eventsExamined":2404,"eventsMatched":1,"bytesRead":35635,
             "unsealedEventsScanned":12,"indexes":[{"field":"timestamp","kind":"time"},{"field":"traceId","kind":"bloom"}]},
             "continuationToken":null}
            """;

        var d = Workload.ParseDiagnostics(Encoding.UTF8.GetBytes(body));

        Assert.NotNull(d);
        Assert.Equal(1.25, d.ElapsedMs);
        Assert.Equal(4, d.SegmentsConsidered);
        Assert.Equal(3, d.SegmentsSkipped);
        Assert.Equal(2, d.BlocksRead);
        Assert.Equal(19, d.BlocksSkipped);
        Assert.Equal(2404, d.EventsExamined);
        Assert.Equal(1, d.EventsMatched);
        Assert.Equal(35635, d.BytesRead);
        Assert.Equal(12, d.UnsealedEventsScanned);
        Assert.Equal(["timestamp:time", "traceId:bloom"], d.Indexes);
    }

    [Theory]
    [InlineData("""{"count":3}""")]
    [InlineData("""[1,2]""")]
    [InlineData("not json")]
    [InlineData("")]
    public void Missing_or_malformed_diagnostics_are_null(string body) =>
        Assert.Null(Workload.ParseDiagnostics(Encoding.UTF8.GetBytes(body)));

    [Theory]
    [InlineData("""{"accepted":800}""", 800)]
    [InlineData("""{"error":"rate_limited"}""", 0)]
    [InlineData("oops", 0)]
    public void Reads_the_accepted_count(string body, long expected) =>
        Assert.Equal(expected, Workload.ParseAccepted(Encoding.UTF8.GetBytes(body)));
}
