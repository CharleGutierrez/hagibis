/**
 * Hagibis Cloudflare Worker Edge OpenTelemetry Integration (`worker_otel_telemetry.ts`)
 * ==============================================================================
 * Polyglot Observability Mesh for Cloudflare Workers Edge Gateway (`worker/index.ts`)
 *
 * Implements:
 * - Strict W3C Trace Context recommendation (W3C traceparent + tracestate)
 * - Sub-millisecond high-resolution span recording and event attachment
 * - Edge-to-Firecracker MicroVM context propagation over QUIC tunnel / HTTP dispatch
 * - Zero-copy telemetry buffering and batch export to Cloudflare KV (`HAGIBIS_EDGE_KV`)
 *   and D1 Distributed SQL (`HAGIBIS_D1_DB`)
 * - Transparent middleware wrapper (`withEdgeTracing`) for turnkey route instrumentation
 */

export interface TraceContext {
  version: string;
  traceId: string;
  parentSpanId?: string;
  spanId: string;
  traceFlags: string;
  traceState?: string;
}

export type SpanStatusCode = "UNSET" | "OK" | "ERROR";

export interface SpanStatus {
  code: SpanStatusCode;
  description?: string;
}

export interface SpanEvent {
  name: string;
  timestampMs: number;
  attributes?: Record<string, unknown>;
}

export interface SpanLink {
  traceId: string;
  spanId: string;
  attributes?: Record<string, unknown>;
}

export interface CompletedEdgeSpan {
  traceId: string;
  spanId: string;
  parentSpanId?: string;
  name: string;
  serviceName: string;
  kind: "SERVER" | "CLIENT" | "INTERNAL" | "PRODUCER" | "CONSUMER";
  startTimeMs: number;
  endTimeMs: number;
  durationMs: number;
  status: SpanStatus;
  attributes: Record<string, unknown>;
  events: SpanEvent[];
  links: SpanLink[];
  traceparent: string;
}

// ==============================================================================
// 1. W3C Trace Context Carrier Utilities
// ==============================================================================

/**
 * Generates high-entropy lowercase hex string of specified byte length
 */
export function generateHexId(byteLength: number): string {
  const bytes = new Uint8Array(byteLength);
  crypto.getRandomValues(bytes);
  // Ensure non-zero
  if (bytes.every((b) => b === 0)) {
    bytes[0] = 0x01;
  }
  return Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

/**
 * Generates a new sampled root W3C trace context
 */
export function generateRootTraceContext(): TraceContext {
  return {
    version: "00",
    traceId: generateHexId(16), // 32 hex chars
    spanId: generateHexId(8),   // 16 hex chars
    traceFlags: "01",           // Sampled
  };
}

/**
 * Extracts and strictly validates W3C traceparent header:
 * Format: 00-{32hex_trace_id}-{16hex_parent_id}-{2hex_flags}
 */
export function extractTraceContext(headers: Headers): TraceContext {
  const carrier = headers.get("traceparent") || headers.get("x-trace-parent") || "";
  const traceState = headers.get("tracestate") || undefined;

  if (carrier) {
    const parts = carrier.trim().split("-");
    if (parts.length >= 4) {
      const version = parts[0].toLowerCase();
      const traceId = parts[1].toLowerCase();
      const parentId = parts[2].toLowerCase();
      const traceFlags = parts[3].toLowerCase();

      // Validate W3C constraints
      const isHex = (str: string) => /^[0-9a-f]+$/.test(str);
      const isNotAllZeros = (str: string) => !/^0+$/.test(str);

      if (
        version !== "ff" &&
        traceId.length === 32 &&
        isHex(traceId) &&
        isNotAllZeros(traceId) &&
        parentId.length === 16 &&
        isHex(parentId) &&
        isNotAllZeros(parentId) &&
        traceFlags.length === 2 &&
        isHex(traceFlags)
      ) {
        return {
          version,
          traceId,
          parentSpanId: parentId,
          spanId: generateHexId(8), // Generate fresh span ID for edge processing
          traceFlags,
          traceState,
        };
      }
    }
  }

  // Fallback to fresh root context if traceparent was absent or malformed
  const root = generateRootTraceContext();
  root.traceState = traceState;
  return root;
}

/**
 * Formats trace context into canonical W3C traceparent string:
 * 00-{traceId}-{spanId}-{flags}
 */
export function formatTraceparent(context: TraceContext): string {
  return `${context.version}-${context.traceId}-${context.spanId}-${context.traceFlags}`;
}

/**
 * Injects W3C traceparent into outbound Request or Headers
 */
export function injectTraceContext(context: TraceContext, headers: Headers): Headers {
  headers.set("traceparent", formatTraceparent(context));
  if (context.traceState) {
    headers.set("tracestate", context.traceState);
  }
  return headers;
}

// ==============================================================================
// 2. Active Span & Tracer Implementation
// ==============================================================================

export class EdgeSpan {
  public readonly context: TraceContext;
  public readonly name: string;
  public readonly serviceName: string;
  public readonly kind: CompletedEdgeSpan["kind"];
  public readonly startTimeMs: number;
  private readonly startPerf: number;

  private status: SpanStatus = { code: "UNSET" };
  private attributes: Record<string, unknown> = {};
  private events: SpanEvent[] = [];
  private links: SpanLink[] = [];
  private isFinished = false;

  constructor(
    name: string,
    serviceName: string,
    kind: CompletedEdgeSpan["kind"],
    context?: TraceContext
  ) {
    this.name = name;
    this.serviceName = serviceName;
    this.kind = kind;
    this.context = context || generateRootTraceContext();
    this.startTimeMs = Date.now();
    this.startPerf = performance.now();
  }

  public setAttribute(key: string, value: unknown): this {
    if (!this.isFinished) {
      this.attributes[key] = value;
    }
    return this;
  }

  public setAttributes(attrs: Record<string, unknown>): this {
    if (!this.isFinished) {
      Object.assign(this.attributes, attrs);
    }
    return this;
  }

  public addEvent(name: string, attributes?: Record<string, unknown>): this {
    if (!this.isFinished) {
      this.events.push({
        name,
        timestampMs: Date.now(),
        attributes,
      });
    }
    return this;
  }

  public addLink(traceId: string, spanId: string, attributes?: Record<string, unknown>): this {
    if (!this.isFinished) {
      this.links.push({ traceId, spanId, attributes });
    }
    return this;
  }

  public setStatus(code: SpanStatusCode, description?: string): this {
    if (!this.isFinished) {
      this.status = { code, description };
    }
    return this;
  }

  public getTraceparent(): string {
    return formatTraceparent(this.context);
  }

  public createChildSpan(name: string, kind: CompletedEdgeSpan["kind"] = "INTERNAL"): EdgeSpan {
    const childContext: TraceContext = {
      version: this.context.version,
      traceId: this.context.traceId,
      parentSpanId: this.context.spanId,
      spanId: generateHexId(8),
      traceFlags: this.context.traceFlags,
      traceState: this.context.traceState,
    };
    return new EdgeSpan(name, this.serviceName, kind, childContext);
  }

  public finish(statusCode?: SpanStatusCode, errorDesc?: string): CompletedEdgeSpan {
    if (this.isFinished) {
      throw new Error(`Span '${this.name}' is already completed.`);
    }
    this.isFinished = true;

    if (statusCode) {
      this.status = { code: statusCode, description: errorDesc };
    } else if (this.status.code === "UNSET") {
      this.status = { code: "OK" };
    }

    const durationMs = Math.round((performance.now() - this.startPerf) * 1000) / 1000;
    const endTimeMs = this.startTimeMs + Math.round(durationMs);

    return {
      traceId: this.context.traceId,
      spanId: this.context.spanId,
      parentSpanId: this.context.parentSpanId,
      name: this.name,
      serviceName: this.serviceName,
      kind: this.kind,
      startTimeMs: this.startTimeMs,
      endTimeMs,
      durationMs,
      status: this.status,
      attributes: { ...this.attributes },
      events: [...this.events],
      links: [...this.links],
      traceparent: this.getTraceparent(),
    };
  }
}

// ==============================================================================
// 3. Edge Tracer & Telemetry Buffer
// ==============================================================================

export class EdgeTracer {
  private serviceName: string;
  private buffer: CompletedEdgeSpan[] = [];
  private maxBufferSize: number;

  constructor(serviceName = "hagibis-edge-worker", maxBufferSize = 256) {
    this.serviceName = serviceName;
    this.maxBufferSize = maxBufferSize;
  }

  public startSpan(
    name: string,
    kind: CompletedEdgeSpan["kind"] = "SERVER",
    incomingHeaders?: Headers
  ): EdgeSpan {
    const context = incomingHeaders
      ? extractTraceContext(incomingHeaders)
      : generateRootTraceContext();

    return new EdgeSpan(name, this.serviceName, kind, context);
  }

  public recordSpan(span: CompletedEdgeSpan): void {
    this.buffer.push(span);
    if (this.buffer.length > this.maxBufferSize) {
      this.buffer.shift();
    }
  }

  public getBufferedSpans(): CompletedEdgeSpan[] {
    return [...this.buffer];
  }

  public drain(): CompletedEdgeSpan[] {
    const drained = [...this.buffer];
    this.buffer = [];
    return drained;
  }
}

// ==============================================================================
// 4. Edge-to-MicroVM Context Propagation
// ==============================================================================

export interface MicrovmDispatchEnvelope {
  task_id: string;
  tool_or_command: string;
  payload: Record<string, unknown>;
  vcpus: number;
  mem_mib: number;
  timeout_secs: number;
  source_event: string;
  created_at_epoch_ms: number;
  traceparent: string;
  parent_span_id: string;
  edge_trace_id: string;
  telemetry_tags: Record<string, string>;
}

/**
 * Augments a MicrovmDispatchTask with W3C traceparent context and telemetry tags
 * for seamless end-to-end distributed tracing across Edge -> MicroVM.
 */
export function propagateToMicrovmTask(
  task: {
    task_id: string;
    tool_or_command: string;
    payload: Record<string, unknown>;
    vcpus?: number;
    mem_mib?: number;
    timeout_secs?: number;
    source_event?: string;
    created_at_epoch_ms?: number;
  },
  activeSpan: EdgeSpan
): MicrovmDispatchEnvelope {
  activeSpan.addEvent("microvm_dispatch_prepared", {
    task_id: task.task_id,
    tool: task.tool_or_command,
    vcpus: task.vcpus || 2,
    mem_mib: task.mem_mib || 256,
  });

  return {
    task_id: task.task_id,
    tool_or_command: task.tool_or_command,
    payload: task.payload,
    vcpus: task.vcpus || 2,
    mem_mib: task.mem_mib || 256,
    timeout_secs: task.timeout_secs || 30,
    source_event: task.source_event || "edge_http",
    created_at_epoch_ms: task.created_at_epoch_ms || Date.now(),
    traceparent: activeSpan.getTraceparent(),
    parent_span_id: activeSpan.context.spanId,
    edge_trace_id: activeSpan.context.traceId,
    telemetry_tags: {
      "edge.service": activeSpan.serviceName,
      "edge.span_name": activeSpan.name,
      "w3c.version": activeSpan.context.version,
    },
  };
}

// ==============================================================================
// 5. Cloudflare Workers KV & D1 Exporters
// ==============================================================================

export interface EdgeTelemetryEnv {
  HAGIBIS_EDGE_KV?: {
    put: (key: string, value: string, options?: { expirationTtl?: number }) => Promise<void>;
    get: (key: string) => Promise<string | null>;
  };
  HAGIBIS_D1_DB?: {
    prepare: (query: string) => {
      bind: (...args: any[]) => {
        run: () => Promise<unknown>;
      };
    };
  };
}

/**
 * Asynchronously persists completed spans into KV and D1 without blocking edge response
 */
export async function exportEdgeTelemetry(
  spans: CompletedEdgeSpan[],
  env: EdgeTelemetryEnv
): Promise<void> {
  if (!spans || spans.length === 0) return;

  const exportPromises: Promise<unknown>[] = [];

  // 1. High-frequency KV snapshot
  if (env.HAGIBIS_EDGE_KV) {
    const latestSummary = {
      exported_at: Date.now(),
      count: spans.length,
      latest_spans: spans.slice(-10),
    };
    exportPromises.push(
      env.HAGIBIS_EDGE_KV.put("edge_telemetry:latest", JSON.stringify(latestSummary), {
        expirationTtl: 3600,
      }).catch(() => {})
    );
  }

  // 2. D1 SQL Event Journal
  if (env.HAGIBIS_D1_DB) {
    for (const span of spans) {
      exportPromises.push(
        env.HAGIBIS_D1_DB.prepare(
          `INSERT INTO edge_task_journal
           (entry_id, task_id, agent_role, tool_invoked, status, duration_ms, output_summary, created_at)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?)`
        )
          .bind(
            `span_${span.spanId}_${span.startTimeMs}`,
            span.traceId,
            span.serviceName,
            span.name,
            span.status.code,
            Math.round(span.durationMs),
            JSON.stringify({
              traceparent: span.traceparent,
              attributes: span.attributes,
              events_count: span.events.length,
            }),
            new Date(span.startTimeMs).toISOString()
          )
          .run()
          .catch(() => {})
      );
    }
  }

  await Promise.allSettled(exportPromises);
}

// ==============================================================================
// 6. Transparent Route Instrumentation Wrapper (`withEdgeTracing`)
// ==============================================================================

export interface ExecutionContextLike {
  waitUntil: (promise: Promise<any>) => void;
}

/**
 * Higher-order function wrapping edge route handlers with automated W3C tracing
 */
export async function withEdgeTracing(
  request: Request,
  env: EdgeTelemetryEnv,
  ctx: ExecutionContextLike | undefined,
  spanName: string,
  handler: (span: EdgeSpan) => Promise<Response>
): Promise<Response> {
  const tracer = new EdgeTracer("hagibis-edge-core");
  const span = tracer.startSpan(spanName, "SERVER", request.headers);

  // Set HTTP attributes
  const url = new URL(request.url);
  span.setAttributes({
    "http.method": request.method,
    "http.url": request.url,
    "http.pathname": url.pathname,
    "http.user_agent": request.headers.get("user-agent") || "unknown",
    "cf.colo": (request as any).cf?.colo || "unknown",
  });

  try {
    const response = await handler(span);

    span.setAttribute("http.status_code", response.status);
    if (response.status >= 500) {
      span.setStatus("ERROR", `HTTP ${response.status}`);
    } else {
      span.setStatus("OK");
    }

    const completed = span.finish();
    tracer.recordSpan(completed);

    // Background flush via ctx.waitUntil if available
    const flushPromise = exportEdgeTelemetry([completed], env);
    if (ctx && typeof ctx.waitUntil === "function") {
      ctx.waitUntil(flushPromise);
    }

    // Attach W3C traceparent to response headers for downstream callers
    const headers = new Headers(response.headers);
    headers.set("traceparent", completed.traceparent);
    headers.set("x-trace-id", completed.traceId);

    return new Response(response.body, {
      status: response.status,
      statusText: response.statusText,
      headers,
    });
  } catch (err: unknown) {
    const errorMsg = err instanceof Error ? err.message : String(err);
    span.setStatus("ERROR", errorMsg);
    span.addEvent("exception", { message: errorMsg });

    const completed = span.finish("ERROR", errorMsg);
    tracer.recordSpan(completed);

    const flushPromise = exportEdgeTelemetry([completed], env);
    if (ctx && typeof ctx.waitUntil === "function") {
      ctx.waitUntil(flushPromise);
    }

    throw err;
  }
}
