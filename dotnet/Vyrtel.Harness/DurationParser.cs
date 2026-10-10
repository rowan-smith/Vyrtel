using System.Globalization;

namespace Vyrtel.Harness;

/// <summary>Parses durations like <c>250ms</c>, <c>30s</c>, <c>5m</c> or <c>1h</c> for CLI options.</summary>
public static class DurationParser
{
    public static TimeSpan Parse(string text)
    {
        text = text.Trim();
        double multiplier;
        string number;
        if (text.EndsWith("ms", StringComparison.OrdinalIgnoreCase))
        {
            number = text[..^2];
            multiplier = 0.001;
        }
        else if (text.EndsWith('s'))
        {
            number = text[..^1];
            multiplier = 1.0;
        }
        else if (text.EndsWith('m'))
        {
            number = text[..^1];
            multiplier = 60.0;
        }
        else if (text.EndsWith('h'))
        {
            number = text[..^1];
            multiplier = 3600.0;
        }
        else
        {
            number = text;
            multiplier = 1.0;
        }

        if (!double.TryParse(number, NumberStyles.Float, CultureInfo.InvariantCulture, out var value))
        {
            throw new FormatException($"invalid duration '{text}' (try 250ms, 30s, 5m or 1h)");
        }
        return TimeSpan.FromSeconds(value * multiplier);
    }
}
