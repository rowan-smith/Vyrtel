using Vyrtel.Harness;

namespace Vyrtel.Baseline.Tests;

/// <summary>Review finding: a run must not measure queries against an incomplete dataset.</summary>
public class DatasetCheckTests
{
    private static readonly SignalCounts Fixture = new(100_000, 3_000, 100);

    [Fact]
    public void Counts_spans_and_data_points_from_the_fixture_payloads()
    {
        var factory = new EventFactory(seed: 42);

        Assert.Equal(3, DatasetCheck.CountItems(factory.TracePayload(), "spans"));
        Assert.Equal(37, DatasetCheck.CountItems(factory.OtlpMetrics(37), "dataPoints"));
    }

    [Fact]
    public void Counts_arrays_at_any_depth()
    {
        var json = """{"a":{"spans":[1,2]},"b":[{"spans":[3]},{"c":{"spans":[]}}],"spans":"not an array"}"""u8.ToArray();

        Assert.Equal(3, DatasetCheck.CountItems(json, "spans"));
    }

    [Fact]
    public void A_complete_dataset_passes() =>
        Assert.Null(DatasetCheck.Verify(Fixture, Fixture, Fixture));

    [Fact]
    public void Events_acknowledged_but_not_stored_fail_the_run()
    {
        var problem = DatasetCheck.Verify(Fixture, Fixture, Fixture with { Logs = 99_500 });

        Assert.NotNull(problem);
        Assert.StartsWith("the seeded dataset is incomplete, so no baseline was recorded", problem);
        Assert.Contains("logs: 99,500 stored, expected 100,000", problem);
    }

    [Fact]
    public void Under_acknowledged_events_fail_the_run()
    {
        var problem = DatasetCheck.Verify(Fixture, Fixture with { Logs = 99_999 }, Fixture);

        Assert.Contains("logs: the server acknowledged 99,999 of 100,000", problem);
    }

    [Theory]
    [InlineData(2_999, 100, "spans: 2,999 stored, expected 3,000")]
    [InlineData(3_000, 99, "metric points: 99 stored, expected 100")]
    [InlineData(3_001, 100, "spans: 3,001 stored, expected 3,000")] // a surplus is as wrong as a loss
    public void Every_signal_is_checked(long spans, long points, string expected) =>
        Assert.Contains(expected, DatasetCheck.Verify(Fixture, Fixture, Fixture with { Traces = spans, Metrics = points }));

    [Fact]
    public void Reads_stored_counts_from_system_info()
    {
        const string info = """{"name":"Vyrtel","eventCounts":{"logs":100000,"traces":3000,"metrics":100},"storageBytes":1}""";

        Assert.Equal(Fixture, DatasetCheck.ParseStored(info));
    }
}
