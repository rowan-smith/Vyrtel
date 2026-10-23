namespace Vyrtel.Baseline.Tests;

/// <summary>
/// Review finding: the query statistics are cumulative since the server started, so hotspots
/// must be the difference of snapshots taken around the measured mixed-load window.
/// </summary>
public class HotspotsTests
{
    private static string Stats(params (string Field, string Index, long Queries, long Bytes, long Scanned, long Skipped, double Ms)[] rows) =>
        System.Text.Json.JsonSerializer.Serialize(new
        {
            fields = rows.Select(r => new
            {
                signal = "logs",
                field = r.Field,
                index = r.Index,
                queries = r.Queries,
                bytesRead = r.Bytes,
                segmentsScanned = r.Scanned,
                segmentsSkipped = r.Skipped,
                totalMs = r.Ms,
                lastSeen = 1,
            }),
        });

    [Fact]
    public void Only_the_window_counts_ranked_by_query_time()
    {
        var before = Stats(("level", "bitmap", 100, 1000, 10, 90, 50.0), ("customerId", "bloom", 5, 500, 1, 4, 5.0));
        var after = Stats(
            ("level", "bitmap", 110, 1600, 14, 96, 54.0), // +10 queries, +4 ms
            ("customerId", "bloom", 5, 500, 1, 4, 5.0), // not queried in the window
            ("message", "none", 3, 3000, 6, 0, 30.0)); // first queried in the window

        var h = Hotspots.Diff(before, after);

        Assert.Equal(["message", "level"], h.Select(x => x.Field));
        Assert.Equal(new FieldHotspot("logs", "message", "none", 3, 3000, 6, 0, 30.0), h[0]);
        var level = h[1];
        Assert.Equal((10L, 600L, 4L, 6L), (level.Queries, level.BytesRead, level.SegmentsScanned, level.SegmentsSkipped));
        Assert.Equal(4.0, level.TotalMs, 6);
        Assert.Equal(0.4, level.AvgMs, 6);
        Assert.Equal(0.6, level.SkippedRatio, 6);
    }

    [Fact]
    public void Keeps_the_top_rows_and_tolerates_a_response_without_fields()
    {
        var after = Stats(Enumerable.Range(0, Hotspots.Top + 5).Select(i => ($"f{i}", "none", 1L, 0L, 1L, 0L, (double)i)).ToArray());

        var h = Hotspots.Diff("{}", after);

        Assert.Equal(Hotspots.Top, h.Count);
        Assert.Equal($"f{Hotspots.Top + 4}", h[0].Field);
        Assert.Empty(Hotspots.Parse("""{"error":{"code":"unauthorized"}}"""));
    }

    [Fact]
    public void The_report_lists_the_first_runs_hotspots()
    {
        var run = Fixtures.Run(1, 1.0);
        var withHotspots = run with
        {
            Mixed = run.Mixed with { Hotspots = [new FieldHotspot("logs", "level", "bitmap", 10, 600, 4, 6, 4.0)] },
        };

        var md = BaselineReportWriter.ToMarkdown(Fixtures.Report([withHotspots]));
        var without = BaselineReportWriter.ToMarkdown(Fixtures.Report([run]));

        Assert.Contains("## Index hotspots in the mixed load (run 1)", md);
        Assert.Contains("| `level` | logs | bitmap | 10 | 0.400 ms | 600 B | 4 | 60% |", md);
        Assert.DoesNotContain("Index hotspots", without);
    }
}
