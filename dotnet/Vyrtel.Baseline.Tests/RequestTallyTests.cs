namespace Vyrtel.Baseline.Tests;

/// <summary>AC1: accepted and rejected requests are counted separately, by reason.</summary>
public class RequestTallyTests
{
    [Theory]
    [InlineData(200, Outcome.Accepted)]
    [InlineData(204, Outcome.Accepted)]
    [InlineData(429, Outcome.Backpressure)]
    [InlineData(503, Outcome.Backpressure)]
    [InlineData(413, Outcome.TooLarge)]
    [InlineData(400, Outcome.Rejected)]
    [InlineData(415, Outcome.Rejected)]
    [InlineData(500, Outcome.Rejected)]
    [InlineData(0, Outcome.TransportError)]
    public void Classifies_each_status(int status, Outcome expected) =>
        Assert.Equal(expected, RequestTally.Classify(status));

    [Fact]
    public void Snapshot_counts_by_outcome_and_status()
    {
        var tally = new RequestTally();
        foreach (var status in new[] { 200, 200, 200, 429, 503, 413, 500, 0 })
        {
            tally.Add(status);
        }

        var c = tally.Snapshot();

        Assert.Equal(8, c.Total);
        Assert.Equal(3, c.Accepted);
        Assert.Equal(2, c.RejectedBackpressure);
        Assert.Equal(1, c.RejectedTooLarge);
        Assert.Equal(1, c.RejectedOther);
        Assert.Equal(1, c.TransportErrors);
        Assert.Equal(4, c.Rejected);
        Assert.Equal(3, c.ByStatus["200"]);
        Assert.Equal(1, c.ByStatus["transport"]);
    }

    [Fact]
    public async Task Counts_concurrent_adds_exactly()
    {
        var tally = new RequestTally();
        await Task.WhenAll(Enumerable.Range(0, 8).Select(_ => Task.Run(() =>
        {
            for (var i = 0; i < 10_000; i++) tally.Add(i % 10 == 0 ? 429 : 200);
        })));

        var c = tally.Snapshot();

        Assert.Equal(80_000, c.Total);
        Assert.Equal(8_000, c.RejectedBackpressure);
    }

    [Fact]
    public void Sum_merges_operations()
    {
        var sum = RequestTally.Sum([Fixtures.Counts((200, 5), (429, 1)), Fixtures.Counts((200, 2), (0, 3))]);

        Assert.Equal(11, sum.Total);
        Assert.Equal(7, sum.Accepted);
        Assert.Equal(1, sum.RejectedBackpressure);
        Assert.Equal(3, sum.TransportErrors);
    }
}
