namespace Vyrtel.Harness;

/// <summary>Human-readable byte counts for reports and rollups.</summary>
public static class ByteFormat
{
    private static readonly string[] Units = ["B", "KB", "MB", "GB", "TB"];

    public static string Format(long bytes) => Format((double)bytes, decimals: bytes < 10240 ? 0 : 1);

    public static string Format(double bytes, int decimals = 1)
    {
        var value = bytes;
        var unit = 0;
        while (value >= 1024 && unit < Units.Length - 1)
        {
            value /= 1024;
            unit++;
        }
        return $"{value.ToString($"N{decimals}", System.Globalization.CultureInfo.InvariantCulture)} {Units[unit]}";
    }
}
