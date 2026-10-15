namespace Vyrtel.Harness;

/// <summary>Status code and body of an HTTP response, whatever the status.</summary>
public sealed record RawResponse(int Status, byte[] Body)
{
    public bool IsSuccess => Status is >= 200 and < 300;
}
