---
name: observability-otel
description: Enterprise distributed tracing, structured logging, and OpenTelemetry telemetry
version: 0.1.0
category: observability
---

# Enterprise Observability & OpenTelemetry Tracing

## Overview
The `observability-otel` skill standardizes structured distributed tracing, high-throughput JSON logging, and OpenTelemetry telemetry across all Hagibis workspace crates (`hgb-core`, `hgb-daemon`, and `hgb-cli`).

## Span Hierarchies & Trace Context
Distributed spans follow the OpenTelemetry trace model:
- **Trace ID**: 128-bit unique hexadecimal identifier for an end-to-end distributed transaction.
- **Span ID**: 64-bit unique hexadecimal identifier for individual units of work within a trace.
- **Parent Span ID**: 64-bit identifier linking child spans to their caller/parent in the hierarchy.
- **Trace Flags**: 8-bit flags specifying sampling decisions (e.g., `01` for sampled).

```
[CLI Command Ingestion Span] (trace_id: 4bf92f3577b34da6a3ce929d0e0e4736)
  └── [IPC Transport Dispatch] (parent_id: 00f067aa0ba902b7)
        └── [Daemon Request Execution] (parent_id: 5fb397be34d23b26)
              ├── [MicroVM Jail Execution] (parent_id: b7ad6b716988e4d3)
              └── [Storage / Checkpoint Persistence] (parent_id: b7ad6b716988e4d3)
```

## W3C Traceparent Header Specification
Context propagation between processes (CLI <-> Daemon Unix Socket, MCP SSE events, MicroVM IPC) strictly implements the W3C Trace Context recommendation (`traceparent` header):

```
traceparent: {version}-{trace_id}-{parent_id}-{trace_flags}
Example: 00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01
```

- **version**: `00` (Current W3C specification)
- **trace_id**: 32 lowercase hexadecimal characters (16 bytes)
- **parent_id**: 16 lowercase hexadecimal characters (8 bytes)
- **trace_flags**: 2 lowercase hexadecimal characters (1 byte, `01` = recorded/sampled)

## Tracing Subscriber & JSON Formatter
The telemetry system configures a unified `tracing-subscriber` layer supporting:
- Level filtering via `RUST_LOG` environment variable (defaults to `info` if unset).
- Structured JSON output format for high-throughput ingestion into SIEM / log aggregators.
- Span lifecycle events (`NEW`, `CLOSE`, `ENTER`, `EXIT`).

## Local OTLP Trace Buffer & In-Memory Ring Buffer
- **In-Memory Ring Buffer (`ActiveTelemetryBuffer`)**: Thread-safe circular buffer capturing completed spans with timestamp, duration, status code, and arbitrary key-value attributes.
- **Local Exporter (`OtlpTraceExporter`)**: Serializes recorded spans to local disk (`/tmp/hgb-traces.jsonl`) or exports via OTLP JSON over HTTP streaming collector.

## Dashboard Telemetry Metrics
Exposes real-time span metrics:
- Span duration percentiles (p50, p95, p99).
- Error rates and failure categorizations.
- In-flight active span counts.
- Cross-process propagation latency.
