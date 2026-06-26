# agentos-otel

`agentos-otel` is a sidecar that tails `flight.jsonl` and exports AgentOS
agent runs as OpenTelemetry traces to any OTLP-compatible backend (Jaeger,
Grafana Tempo, Honeycomb, etc.).

It requires no changes to running agents — the flight recorder already
captures everything. This tool is pure translation from AgentOS's internal
JSONL format to the OpenTelemetry standard.

## Quick start (Docker Compose + Jaeger)

```bash
# Start Jaeger
docker compose -f docker/otel-compose.yml up -d

# Run an agent (writes flight.jsonl)
export ANTHROPIC_API_KEY=sk-...
cargo run -- agentd/agent.toml

# Export traces (in another terminal)
FLIGHT_LOG_PATH=$(pwd)/flight.jsonl \
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318 \
cargo run -p agentos-otel -- $(pwd)/flight.jsonl

# Open http://localhost:16686 → search service: agentos
```

## Standalone binary

```bash
FLIGHT_LOG_PATH=/path/to/flight.jsonl \
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318 \
./agentos-otel
```

Or pass the path as a positional argument:

```bash
./agentos-otel /path/to/flight.jsonl
```

## Environment variables

| Variable | Default | Description |
|---|---|---|
| `FLIGHT_LOG_PATH` | *(required)* | Absolute path to `flight.jsonl`. Must end in `.jsonl`. |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | `http://localhost:4318` | OTLP endpoint. Must be `http://` or `https://`. |
| `OTEL_EXPORT_PROTOCOL` | `http/protobuf` | `http/protobuf` or `grpc` |
| `OTEL_SERVICE_NAME` | `agentos` | Service name shown in Jaeger/Tempo/Honeycomb |
| `OTEL_TAIL_FROM_BEGINNING` | `false` | Set `true` to replay the entire file from offset 0 |
| `OTEL_POLL_INTERVAL_MS` | `500` | File poll interval in milliseconds |
| `OTEL_IDLE_TIMEOUT_SECS` | `30` | Force-close open spans after N seconds of inactivity |
| `OTEL_REDACT_PREVIEWS` | `false` | Set `true` to strip `*.preview` span attributes before export |
| `OTEL_SESSION_ID` | *(unset)* | Optional session label added to all spans |

## Trace model

```
agentd.run (trace root — one per agentd invocation)
  └── agent.<id>     (one per agent)
        └── gen_ai.chat   (one per inference turn)
              └── tool.<name>  (one per tool call)
```

The run span's trace ID is derived from the `run_id` UUID in the
`scheduler_started` flight event, giving a stable identifier across
restarts.

## Privacy

Set `OTEL_REDACT_PREVIEWS=true` to strip task previews, tool input/output
previews, and final answer previews before export. Useful when sending
traces to third-party backends.

## Log rotation

The tailer tracks `(device, inode, offset)`. It detects both rename-based
log rotation (new inode) and copy-truncate rotation (file shorter than
remembered offset), seeking to offset 0 in either case.

## GenAI semantic conventions

Inference spans carry `gen_ai.*` attributes per
[OTel GenAI semconv v1.29.0](https://opentelemetry.io/docs/specs/semconv/gen-ai/):

- `gen_ai.system` = `anthropic`
- `gen_ai.request.model` — model ID from the inference request
- `gen_ai.usage.input_tokens` — from the inference response
- `gen_ai.usage.output_tokens` — from the inference response
