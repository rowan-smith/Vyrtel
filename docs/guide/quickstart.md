# Quickstart

Run Vyrtel, send it an event and find that event again. You need Docker, or one of the
[release binaries](installation.md).

## 1. Start Vyrtel

```bash
docker run -p 8080:8080 -v vyrtel-data:/data ghcr.io/rowan-smith/vyrtel:latest
```

Vyrtel prints what it is listening on, then `Ready.`:

```text
Web UI              http://localhost:8080
OTLP HTTP           http://0.0.0.0:8080/v1/*
Admin login         admin / 3f9c2a7d41b08e6c5a2d9f10
                    (generated once; set auth.admin_password to choose your own)

Ready.
```

Copy the admin password. It is printed only on the first start.

## 2. Sign in

Open <http://localhost:8080> and sign in as `admin` with that password.

## 3. Send an event

```bash
curl -X POST http://localhost:8080/api/v1/events \
  -H 'content-type: application/json' \
  -d '{
    "level": "Error",
    "service": "payments",
    "message": "Payment failed",
    "properties": { "customerId": 481, "amount": 71.50 },
    "exception": { "type": "TimeoutException", "message": "Payment provider timed out" }
  }'
```

The response is `{"accepted":1}`. A new install accepts events without a key, so producers
work straight away. See [Security](security.md) to require one.

## 4. Find it

In **Logs**, type a query and press Enter:

```text
level = "Error" and service = "payments"
```

Click the event to see its properties and exception. Other queries to try:

```text
customerId = 481
amount > 50
"failed"
```

## Next steps

- [Send data with OpenTelemetry](opentelemetry.md) from your own services.
- [Generate test data](https://github.com/rowan-smith/Vyrtel/tree/main/tools/loadgen):
  `cargo run --release -p loadgen -- --rate 1000 --duration 60s --traces-every 200`.
- Learn the [query language](../query-language.md).
- [Create an ingest key](security.md#protect-ingestion) before exposing Vyrtel to a network.
