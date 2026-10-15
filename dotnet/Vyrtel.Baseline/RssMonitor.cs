using System.Diagnostics;

namespace Vyrtel.Baseline;

/// <summary>
/// Tracks a server process's resident set size. The OS keeps its own high-water mark
/// (PeakWorkingSet on Windows, VmHWM on Linux), which catches spikes between samples; where
/// it is unavailable (macOS reports 0) the 100 ms samples are the fallback.
/// </summary>
public sealed class RssMonitor : IAsyncDisposable
{
    public const string OsPeakSource = "os-peak";
    public const string SampledSource = "sampled-100ms";

    private readonly Process _process;
    private readonly CancellationTokenSource _stop = new();
    private readonly Task _loop;
    private long _sampledMax;

    public RssMonitor(Process process)
    {
        _process = process;
        Current = Sample();
        _loop = Task.Run(async () =>
        {
            try
            {
                while (!_stop.IsCancellationRequested)
                {
                    await Task.Delay(100, _stop.Token);
                    Sample();
                }
            }
            catch (OperationCanceledException)
            {
                // stopped
            }
        });
    }

    /// <summary>RSS at the most recent sample, in bytes.</summary>
    public long Current { get; private set; }

    private long Sample()
    {
        try
        {
            _process.Refresh();
            var rss = _process.WorkingSet64;
            Current = rss;
            if (rss > Interlocked.Read(ref _sampledMax))
            {
                Interlocked.Exchange(ref _sampledMax, rss);
            }
            return rss;
        }
        catch
        {
            return 0; // exited
        }
    }

    /// <summary>Stops sampling and returns the peak. Call while the process is still running.</summary>
    public async Task<(long PeakBytes, string Source)> StopAsync()
    {
        Sample();
        await _stop.CancelAsync();
        await _loop;
        long osPeak = 0;
        try
        {
            _process.Refresh();
            osPeak = _process.PeakWorkingSet64;
        }
        catch
        {
            // exited or unsupported
        }
        return Choose(osPeak, Interlocked.Read(ref _sampledMax));
    }

    /// <summary>The OS peak when it is at least the sampled maximum (it should always be); otherwise the samples.</summary>
    public static (long PeakBytes, string Source) Choose(long osPeak, long sampledMax) =>
        osPeak > 0 && osPeak >= sampledMax ? (osPeak, OsPeakSource) : (sampledMax, SampledSource);

    public async ValueTask DisposeAsync()
    {
        if (!_stop.IsCancellationRequested)
        {
            await _stop.CancelAsync();
            await _loop;
        }
        _stop.Dispose();
    }
}
