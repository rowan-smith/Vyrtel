using System.Diagnostics;
using System.Text;

namespace Vyrtel.LiveDataSim.Services;

public static class ExceptionFormatter
{
    /// <summary>
    /// Full exception text including type, message, stack frames, data, and all inner / aggregate exceptions.
    /// </summary>
    public static string Format(Exception exception)
    {
        var sb = new StringBuilder(2048);
        WriteException(sb, exception, depth: 0, label: null);
        return sb.ToString();
    }

    static void WriteException(StringBuilder sb, Exception exception, int depth, string? label)
    {
        var indent = new string(' ', depth * 2);
        if (!string.IsNullOrEmpty(label))
        {
            sb.Append(indent).Append(label).AppendLine(":");
        }

        sb.Append(indent)
            .Append(exception.GetType().FullName)
            .Append(": ")
            .AppendLine(exception.Message);

        if (!string.IsNullOrEmpty(exception.Source))
        {
            sb.Append(indent).Append("Source: ").AppendLine(exception.Source);
        }

        if (exception.TargetSite is not null)
        {
            sb.Append(indent)
                .Append("TargetSite: ")
                .Append(exception.TargetSite.DeclaringType?.FullName)
                .Append('.')
                .AppendLine(exception.TargetSite.Name);
        }

        if (exception.HResult != 0)
        {
            sb.Append(indent).Append("HResult: 0x").AppendLine(exception.HResult.ToString("X8"));
        }

        if (exception.Data.Count > 0)
        {
            sb.Append(indent).AppendLine("Data:");
            foreach (var key in exception.Data.Keys)
            {
                sb.Append(indent)
                    .Append("  [")
                    .Append(key)
                    .Append("] = ")
                    .AppendLine(exception.Data[key]?.ToString() ?? "null");
            }
        }

        var stack = exception.StackTrace;
        if (!string.IsNullOrWhiteSpace(stack))
        {
            sb.Append(indent).AppendLine("StackTrace:");
            foreach (var line in stack.Split('\n', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries))
            {
                sb.Append(indent).Append("  ").AppendLine(line);
            }
        }
        else
        {
            // Ensure we still have frames when StackTrace is empty (rethrown without capture).
            sb.Append(indent).AppendLine("StackTrace:");
            sb.Append(indent).AppendLine("  (no stack frames captured)");
        }

        if (exception is AggregateException aggregate)
        {
            var inners = aggregate.Flatten().InnerExceptions;
            for (var i = 0; i < inners.Count; i++)
            {
                sb.AppendLine();
                WriteException(sb, inners[i], depth + 1, $"InnerException[{i}]");
            }
            return;
        }

        if (exception.InnerException is not null)
        {
            sb.AppendLine();
            WriteException(sb, exception.InnerException, depth + 1, "InnerException");
        }
    }
}

public sealed class OrderService(ILogger<OrderService> log, InventoryService inventory, PaymentService payments)
{
    public async Task<object> ListAsync(int customerId, CancellationToken ct)
    {
        log.LogInformation(
            "Listing orders for customer {CustomerId} correlation {CorrelationId}",
            customerId,
            Activity.Current?.Id);
        await Task.Delay(Random.Shared.Next(15, 40), ct);
        var stock = await inventory.CheckSkuAsync("WIDGET", ct);
        log.LogDebug("Inventory check for WIDGET returned {Available}", stock);
        return new[]
        {
            new { id = 42, sku = "WIDGET", total = 19.99, status = "paid", available = stock },
            new { id = 43, sku = "GADGET", total = 8.50, status = "pending", available = true },
        };
    }

    public async Task<object> GetAsync(int orderId, CancellationToken ct)
    {
        log.LogInformation("Loading order {OrderId}", orderId);
        await Task.Delay(Random.Shared.Next(10, 50), ct);
        if (orderId <= 0)
        {
            var ex = BuildOrderNotFound(orderId);
            log.LogError(ex, "Order {OrderId} lookup failed", orderId);
            throw ex;
        }
        return new { id = orderId, sku = "WIDGET", total = 19.99, status = "paid" };
    }

    public async Task<object> CheckoutAsync(int customerId, string sku, int qty, CancellationToken ct)
    {
        log.LogInformation(
            "Checkout started customer {CustomerId} sku {Sku} qty {Quantity}",
            customerId,
            sku,
            qty);
        try
        {
            await inventory.ReserveAsync(sku, qty, ct);
            await payments.ChargeAsync(customerId, sku, qty * 9.99m, ct);
            log.LogInformation("Checkout completed for customer {CustomerId}", customerId);
            return new { ok = true, orderId = Random.Shared.Next(1000, 9999) };
        }
        catch (Exception ex)
        {
            log.LogError(
                ex,
                "Checkout failed for customer {CustomerId} sku {Sku} qty {Quantity}",
                customerId,
                sku,
                qty);
            throw;
        }
    }

    static Exception BuildOrderNotFound(int orderId)
    {
        try
        {
            LookupInRepository(orderId);
            return new InvalidOperationException("unreachable");
        }
        catch (Exception ex)
        {
            return new InvalidOperationException($"Order {orderId} was not found", ex);
        }
    }

    static void LookupInRepository(int orderId) =>
        QueryDatabase(orderId);

    static void QueryDatabase(int orderId) =>
        throw new KeyNotFoundException($"No row for order_id={orderId} in orders table");
}

public sealed class InventoryService(ILogger<InventoryService> log)
{
    public async Task<bool> CheckSkuAsync(string sku, CancellationToken ct)
    {
        log.LogDebug("Checking inventory for {Sku}", sku);
        await Task.Delay(Random.Shared.Next(5, 25), ct);
        return sku != "MISSING";
    }

    public async Task ReserveAsync(string sku, int qty, CancellationToken ct)
    {
        log.LogInformation("Reserving {Quantity} of {Sku}", qty, sku);
        await Task.Delay(Random.Shared.Next(20, 60), ct);
        if (sku.Equals("MISSING", StringComparison.OrdinalIgnoreCase) || qty > 50)
        {
            throw BuildStockException(sku, qty);
        }
    }

    static Exception BuildStockException(string sku, int qty)
    {
        try
        {
            AllocateWarehouseBin(sku, qty);
            return new InvalidOperationException("unreachable");
        }
        catch (Exception ex)
        {
            var outer = new InvalidOperationException($"Unable to reserve {qty} × {sku}", ex);
            outer.Data["sku"] = sku;
            outer.Data["qty"] = qty;
            return outer;
        }
    }

    static void AllocateWarehouseBin(string sku, int qty) =>
        CommitReservation(sku, qty);

    static void CommitReservation(string sku, int qty) =>
        throw new InvalidOperationException($"Insufficient stock for {sku} (requested {qty})");
}

public sealed class PaymentService(ILogger<PaymentService> log)
{
    public async Task ChargeAsync(int customerId, string sku, decimal amount, CancellationToken ct)
    {
        log.LogInformation(
            "Charging customer {CustomerId} amount {Amount:0.00} for {Sku} via {Provider}",
            customerId,
            amount,
            sku,
            "stripe");
        await Task.Delay(Random.Shared.Next(40, 120), ct);
        if (amount > 500 || customerId == 13)
        {
            throw BuildPaymentFailure(customerId, amount);
        }
        log.LogInformation("Payment authorized for customer {CustomerId}", customerId);
    }

    public void FailDemo(int customerId)
    {
        try
        {
            CallGateway(customerId);
        }
        catch (Exception ex)
        {
            log.LogError(
                ex,
                "Payment failed for customer {CustomerId} provider {Provider} attempt {Attempt}",
                customerId,
                "stripe",
                3);
            throw;
        }
    }

    static Exception BuildPaymentFailure(int customerId, decimal amount)
    {
        try
        {
            CallGateway(customerId);
            return new InvalidOperationException("unreachable");
        }
        catch (Exception gateway)
        {
            try
            {
                throw new TimeoutException($"Payment gateway timed out for customer {customerId}", gateway);
            }
            catch (Exception timeout)
            {
                var agg = new AggregateException(
                    $"Payment declined for customer {customerId} amount {amount:0.00}",
                    timeout,
                    new InvalidOperationException("Risk engine rejected the charge"));
                agg.Data["customerId"] = customerId;
                agg.Data["amount"] = amount;
                return agg;
            }
        }
    }

    static void CallGateway(int customerId) =>
        StripeClient.Authorize(customerId);
}

static class StripeClient
{
    public static void Authorize(int customerId) =>
        HttpTransport.Post($"/v1/charges?customer={customerId}");
}

static class HttpTransport
{
    public static void Post(string path) =>
        throw new HttpRequestException($"Upstream stripe returned 402 for {path}");
}

public sealed class NotificationService(ILogger<NotificationService> log)
{
    public async Task SendOrderEmailAsync(int orderId, string email, CancellationToken ct)
    {
        log.LogInformation("Queueing order confirmation email for order {OrderId} to {Email}", orderId, email);
        await Task.Delay(Random.Shared.Next(10, 40), ct);
        if (email.Contains("fail", StringComparison.OrdinalIgnoreCase))
        {
            var ex = BuildMailException(orderId, email);
            log.LogError(ex, "Email send failed for order {OrderId} to {Email}", orderId, email);
            throw ex;
        }
        log.LogDebug("Email provider accepted message for {Email}", email);
    }

    static Exception BuildMailException(int orderId, string email)
    {
        try
        {
            SmtpClient.Send(email);
            return new InvalidOperationException("unreachable");
        }
        catch (Exception ex)
        {
            return new InvalidOperationException($"Failed to email order {orderId} to {email}", ex);
        }
    }
}

static class SmtpClient
{
    public static void Send(string email) =>
        throw new IOException($"SMTP connection reset while sending to {email}");
}

public sealed class AuthService(ILogger<AuthService> log)
{
    public async Task<object> LoginAsync(string username, string password, CancellationToken ct)
    {
        log.LogInformation("Login attempt for user {Username}", username);
        await Task.Delay(Random.Shared.Next(20, 80), ct);
        if (string.IsNullOrWhiteSpace(password) || password == "bad")
        {
            var ex = BuildAuthException(username);
            log.LogError(ex, "Login rejected for user {Username} reason {Reason}", username, "invalid_credentials");
            throw ex;
        }
        log.LogInformation("Login succeeded for user {Username}", username);
        return new { token = Guid.NewGuid().ToString("N"), username };
    }

    static Exception BuildAuthException(string username)
    {
        try
        {
            ValidateCredentials(username);
            return new UnauthorizedAccessException("unreachable");
        }
        catch (Exception ex)
        {
            return new UnauthorizedAccessException($"Authentication failed for '{username}'", ex);
        }
    }

    static void ValidateCredentials(string username) =>
        throw new ArgumentException($"Password hash mismatch for user '{username}'");
}
