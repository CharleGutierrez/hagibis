/**
 * Hagibis Worker Asynchronous OpenTelemetry Pipeline Verification Suite
 * ====================================================================
 * Comprehensive verification for Pillar 4: Asynchronous Non-Blocking Telemetry Pipelines:
 *
 * 1. Enqueue is non-blocking (< 0.1ms per span).
 * 2. Batch draining and flushing under load (50 spans/batch).
 * 3. Zero request latency overhead even when export sink fails or hangs.
 * 4. Circuit breaker trips under collector outage and recovers via canary probe.
 * 5. Durable Object / queue buffer memory bounds & drop policies.
 * 6. Turnkey route middleware (`withEdgeTracing`) asynchronous decoupling.
 */

import {
  CompletedEdgeSpan,
  AsyncTelemetryQueue,
  TelemetryBatchBuffer,
  TelemetryCircuitBreaker,
  TelemetryBufferDurableObject,
  getGlobalTelemetryQueue,
  resetGlobalTelemetryQueue,
  withEdgeTracing,
  EdgeSpan,
  EdgeTelemetryEnv,
  ExecutionContextLike,
} from "./worker_otel_telemetry";

function generateTestSpan(index: number): CompletedEdgeSpan {
  const hex32 = "a".repeat(30) + (index % 100).toString().padStart(2, "0");
  const hex16 = "b".repeat(14) + (index % 100).toString().padStart(2, "0");
  return {
    traceId: hex32,
    spanId: hex16,
    name: `test_span_${index}`,
    serviceName: "hagibis-edge-worker-test",
    kind: "SERVER",
    startTimeMs: Date.now(),
    endTimeMs: Date.now() + 5,
    durationMs: 5.123,
    status: { code: "OK" },
    attributes: { "test.index": index, "env": "unit-test" },
    events: [{ name: "test_event", timestampMs: Date.now() }],
    links: [],
    traceparent: `00-${hex32}-${hex16}-01`,
  };
}

let passedTests = 0;
let totalTests = 0;

function assert(condition: boolean, message: string): void {
  if (!condition) {
    console.error(`❌ FAILED: ${message}`);
    throw new Error(`Assertion failed: ${message}`);
  }
  console.log(`  ✓ ${message}`);
}

async function runTestSuite() {
  console.log("================================================================================");
  console.log("PROJECT HAGIBIS - PILLAR 4 ASYNC TELEMETRY BATCHER VERIFICATION");
  console.log("================================================================================\n");

  // Shared mock environment
  const mockEnv: EdgeTelemetryEnv = {
    HAGIBIS_EDGE_KV: {
      put: async (key: string, value: string) => {
        JSON.parse(value);
      },
      get: async () => null,
    },
    HAGIBIS_D1_DB: {
      prepare: (query: string) => ({
        bind: (...args: any[]) => ({
          run: async () => ({ success: true }),
        }),
      }),
      batch: async (statements: any[]) => {
        return statements.map(() => ({ success: true }));
      },
    },
  };

  // ---------------------------------------------------------------------------
  // SUITE 1: Enqueue is Non-Blocking (< 0.1ms latency)
  // ---------------------------------------------------------------------------
  totalTests++;
  console.log("TEST 1: Sub-Millisecond Non-Blocking Enqueue Latency (< 0.1ms)");
  {
    const queue = new AsyncTelemetryQueue({
      maxBufferSize: 20000,
      maxBatchSize: 50,
      flushTimeoutMs: 10000,
    });

    const ITERATIONS = 10000;
    const testSpans = new Array(ITERATIONS);
    for (let i = 0; i < ITERATIONS; i++) {
      testSpans[i] = generateTestSpan(i);
    }

    // Warm up JIT
    for (let i = 0; i < 500; i++) {
      queue.enqueue(testSpans[i]);
    }
    queue.clear();

    const startTime = performance.now();
    for (let i = 0; i < ITERATIONS; i++) {
      const enqueued = queue.enqueue(testSpans[i]);
      if (!enqueued) throw new Error(`Enqueue failed at index ${i}`);
    }
    const totalTimeMs = performance.now() - startTime;
    const avgLatencyMs = totalTimeMs / ITERATIONS;
    const avgLatencyUs = avgLatencyMs * 1000;

    console.log(`  Total time for ${ITERATIONS} enqueues: ${totalTimeMs.toFixed(3)}ms`);
    console.log(`  Average latency per enqueue: ${avgLatencyMs.toFixed(5)}ms (${avgLatencyUs.toFixed(2)}µs)`);

    assert(avgLatencyMs < 0.1, `Average enqueue latency (${avgLatencyMs.toFixed(5)}ms) MUST be < 0.1ms`);
    assert(queue.getStats().bufferSize === ITERATIONS, `Buffer size must equal ${ITERATIONS}`);
    assert(queue.getStats().totalEnqueued === ITERATIONS, `Total enqueued must equal ${ITERATIONS}`);
    passedTests++;
  }

  // ---------------------------------------------------------------------------
  // SUITE 2: Batch Draining and D1/KV Flushing Under Load
  // ---------------------------------------------------------------------------
  totalTests++;
  console.log("\nTEST 2: Batch Draining & Flushing Under High Load (50 spans/batch)");
  {
    const exportedBatches: CompletedEdgeSpan[][] = [];

    const queue = new AsyncTelemetryQueue({
      maxBatchSize: 50,
      flushTimeoutMs: 1000,
      maxBufferSize: 5000,
    });

    queue.setCustomExporter(async (batch) => {
      exportedBatches.push([...batch]);
    });

    // Enqueue 250 spans (should constitute exactly 5 full batches of 50)
    const TOTAL_SPANS = 250;
    for (let i = 0; i < TOTAL_SPANS; i++) {
      queue.enqueue(generateTestSpan(i));
    }

    assert(queue.getStats().bufferSize === 250, "Buffer contains 250 spans prior to flush");

    // Flush batches
    let flushCount = 0;
    while (queue.getStats().bufferSize > 0) {
      const res = await queue.flush(mockEnv);
      assert(res.success, `Batch flush ${flushCount + 1} succeeded`);
      assert(res.exportedCount === 50, `Batch size must be exactly 50 spans (got ${res.exportedCount})`);
      flushCount++;
    }

    assert(flushCount === 5, `Expected 5 batches of 50 spans (got ${flushCount})`);
    assert(exportedBatches.length === 5, "5 exported batches recorded");
    assert(queue.getStats().bufferSize === 0, "Buffer completely drained after flushing");
    assert(queue.getStats().totalExported === 250, "Total exported metric must equal 250");

    // Verify span order and data integrity
    let reconstructedCount = 0;
    let orderValid = true;
    for (let b = 0; b < exportedBatches.length; b++) {
      for (let s = 0; s < exportedBatches[b].length; s++) {
        if (exportedBatches[b][s].name !== `test_span_${reconstructedCount}`) {
          orderValid = false;
        }
        reconstructedCount++;
      }
    }
    assert(orderValid && reconstructedCount === 250, "All 250 spans verified intact in FIFO order");
    passedTests++;
  }

  // ---------------------------------------------------------------------------
  // SUITE 3: Zero Request Latency Overhead Even When Export Sink Fails or Hangs
  // ---------------------------------------------------------------------------
  totalTests++;
  console.log("\nTEST 3: Zero Request Latency Overhead & Circuit Breaker Fault Tolerance");
  {
    // 3a. Hanging export sink (simulating 5000ms network timeout)
    const hangingEnv: EdgeTelemetryEnv = {};
    const hangingQueue = new AsyncTelemetryQueue({
      maxBatchSize: 10,
      flushTimeoutMs: 100,
      maxBufferSize: 1000,
      exportTimeoutMs: 100, // Short export timeout for rapid test
    });

    hangingQueue.setCustomExporter(async () => {
      // Simulate hung network connection
      await new Promise((resolve) => setTimeout(resolve, 5000));
    });

    for (let i = 0; i < 10; i++) {
      hangingQueue.enqueue(generateTestSpan(i));
    }

    // Measure scheduleFlush with ctx.waitUntil
    let waitUntilPromise: Promise<any> | null = null;
    const mockCtx: ExecutionContextLike = {
      waitUntil: (p) => {
        waitUntilPromise = p;
      },
    };

    const startSchedule = performance.now();
    hangingQueue.scheduleFlush(hangingEnv, mockCtx, true);
    const scheduleDurationMs = performance.now() - startSchedule;

    console.log(`  scheduleFlush duration while sink is hanging: ${scheduleDurationMs.toFixed(3)}ms`);
    assert(scheduleDurationMs < 1.0, `scheduleFlush MUST return immediately (< 1.0ms, got ${scheduleDurationMs.toFixed(3)}ms) without awaiting hung network call`);
    assert(waitUntilPromise !== null, "ctx.waitUntil MUST have received the background promise");

    // Wait for the background export to timeout and fail safely
    const flushRes = await waitUntilPromise;
    assert(!flushRes.success, "Flush result must report failure due to timeout");
    assert(hangingQueue.getStats().consecutiveFailures === 1, "Consecutive failures incremented to 1");

    // 3b. Tripping circuit breaker to OPEN after consecutive failures
    const failingQueue = new AsyncTelemetryQueue({
      maxBatchSize: 10,
      circuitBreakerThreshold: 3,
      circuitBreakerCoolDownMs: 200,
      exportTimeoutMs: 50,
    });

    failingQueue.setCustomExporter(async () => {
      throw new Error("Collector 503 Service Unavailable / Connection Refused");
    });

    for (let i = 0; i < 50; i++) {
      failingQueue.enqueue(generateTestSpan(i));
    }

    // Failure 1
    await failingQueue.flush(mockEnv);
    assert(failingQueue.getCircuitBreaker().getState() === "CLOSED", "Circuit breaker closed after 1 failure");

    // Failure 2
    await failingQueue.flush(mockEnv);
    assert(failingQueue.getCircuitBreaker().getState() === "CLOSED", "Circuit breaker closed after 2 failures");

    // Failure 3 -> Trips to OPEN
    await failingQueue.flush(mockEnv);
    assert(failingQueue.getCircuitBreaker().getState() === "OPEN", "Circuit breaker MUST trip to OPEN after 3 failures");
    assert(failingQueue.getStats().circuitBreakerTrippedCount === 1, "Tripped count must be 1");

    // While OPEN, flush is rejected immediately without network call
    const startBreakerOpenFlush = performance.now();
    const openFlushRes = await failingQueue.flush(mockEnv);
    const openFlushDurationMs = performance.now() - startBreakerOpenFlush;

    assert(!openFlushRes.success, "Flush during breaker OPEN must fail fast");
    assert(openFlushDurationMs < 1.0, `Breaker OPEN short-circuit MUST be sub-millisecond (got ${openFlushDurationMs.toFixed(3)}ms)`);
    assert(openFlushRes.circuitBreakerState === "OPEN", "Reported state is OPEN");

    // After cooldown period (200ms), breaker enters HALF_OPEN for canary probe
    await new Promise((resolve) => setTimeout(resolve, 250));
    assert(failingQueue.getCircuitBreaker().getState() === "HALF_OPEN", "Breaker transitions to HALF_OPEN after cooldown");

    // Provide healthy sink for canary recovery
    let canaryAttemptMade = false;
    failingQueue.setCustomExporter(async () => {
      canaryAttemptMade = true;
    });

    // Canary probe 1
    const canary1 = await failingQueue.flush(mockEnv);
    assert(canaryAttemptMade, "Canary attempt was dispatched downstream");
    assert(canary1.success, "Canary batch succeeded");

    // Canary probe 2 -> Fully recovers to CLOSED
    const canary2 = await failingQueue.flush(mockEnv);
    assert(canary2.success, "Second canary batch succeeded");
    assert(failingQueue.getCircuitBreaker().getState() === "CLOSED", "Breaker successfully recovered to CLOSED");
    assert(failingQueue.getStats().consecutiveFailures === 0, "Failures reset to 0 upon recovery");

    passedTests++;
  }

  // ---------------------------------------------------------------------------
  // SUITE 4: Memory Bounds & Drop Policies Under Outage / Overflow
  // ---------------------------------------------------------------------------
  totalTests++;
  console.log("\nTEST 4: Memory Bounds & Drop Policy Prevention of Memory Leaks");
  {
    // Test 4a: drop_oldest policy (default)
    const BUFFER_CAP = 100;
    const dropOldestQueue = new AsyncTelemetryQueue({
      maxBufferSize: BUFFER_CAP,
      dropPolicy: "drop_oldest",
    });

    const TOTAL_INSERTIONS = 500;
    for (let i = 0; i < TOTAL_INSERTIONS; i++) {
      dropOldestQueue.enqueue(generateTestSpan(i));
    }

    const statsOldest = dropOldestQueue.getStats();
    assert(statsOldest.bufferSize === BUFFER_CAP, `Buffer size MUST remain bounded at ${BUFFER_CAP} (got ${statsOldest.bufferSize})`);
    assert(statsOldest.totalDropped === 400, `Dropped count MUST be exactly 400 (got ${statsOldest.totalDropped})`);
    assert(statsOldest.totalEnqueued === TOTAL_INSERTIONS, `Total enqueued count equals ${TOTAL_INSERTIONS}`);

    // Verify retained spans are the newest 100 (indices 400..499)
    const remaining = dropOldestQueue.getBuffer().drainAll();
    assert(remaining.length === BUFFER_CAP, `Drained length must be ${BUFFER_CAP}`);
    assert(remaining[0].name === "test_span_400", `First retained span must be index 400 (got ${remaining[0].name})`);
    assert(remaining[99].name === "test_span_499", `Last retained span must be index 499 (got ${remaining[99].name})`);

    // Test 4b: drop_newest policy
    const dropNewestQueue = new AsyncTelemetryQueue({
      maxBufferSize: BUFFER_CAP,
      dropPolicy: "drop_newest",
    });

    for (let i = 0; i < TOTAL_INSERTIONS; i++) {
      dropNewestQueue.enqueue(generateTestSpan(i));
    }

    const statsNewest = dropNewestQueue.getStats();
    assert(statsNewest.bufferSize === BUFFER_CAP, `Buffer size MUST remain bounded at ${BUFFER_CAP}`);
    assert(statsNewest.totalDropped === 400, `Dropped count MUST be exactly 400`);

    // Verify retained spans are the first 100 (indices 0..99)
    const remainingNewest = dropNewestQueue.getBuffer().drainAll();
    assert(remainingNewest[0].name === "test_span_0", `First retained span must be index 0 (got ${remainingNewest[0].name})`);
    assert(remainingNewest[99].name === "test_span_99", `Last retained span must be index 99 (got ${remainingNewest[99].name})`);

    passedTests++;
  }

  // ---------------------------------------------------------------------------
  // SUITE 5: Durable Object Stateful Telemetry Buffer Integration
  // ---------------------------------------------------------------------------
  totalTests++;
  console.log("\nTEST 5: TelemetryBufferDurableObject Integration & Alarm Flushing");
  {
    let alarmTimeSet: number | null = null;
    const mockStorage = {
      setAlarm: async (time: number) => {
        alarmTimeSet = time;
      },
      getAlarm: async () => alarmTimeSet,
      deleteAlarm: async () => {
        alarmTimeSet = null;
      },
    };
    const mockState = { storage: mockStorage };
    const mockEnv: EdgeTelemetryEnv = {
      HAGIBIS_EDGE_KV: {
        put: async () => {},
        get: async () => null,
      },
    };

    const telemetryDO = new TelemetryBufferDurableObject(mockState, mockEnv);

    // 1. Ingest spans via POST /enqueue
    const spansToEnqueue = [generateTestSpan(1), generateTestSpan(2), generateTestSpan(3)];
    const enqueueReq = new Request("https://edge.hagibis.internal/api/telemetry/enqueue", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ spans: spansToEnqueue }),
    });
    const enqueueRes = await telemetryDO.fetch(enqueueReq);
    assert(enqueueRes.status === 200, "Enqueue endpoint returned 200 OK");
    const enqueueBody: any = await enqueueRes.json();
    assert(enqueueBody.enqueued_count === 3, "Reported 3 spans enqueued");
    assert(enqueueBody.buffer_size === 3, "Reported buffer size 3");
    assert(alarmTimeSet !== null, "DO alarm was armed on span ingestion");

    // 2. Query stats via GET /stats
    const statsReq = new Request("https://edge.hagibis.internal/api/telemetry/stats", { method: "GET" });
    const statsRes = await telemetryDO.fetch(statsReq);
    assert(statsRes.status === 200, "Stats endpoint returned 200 OK");
    const statsBody: any = await statsRes.json();
    assert(statsBody.bufferSize === 3, "Stats buffer size is 3");
    assert(statsBody.circuitBreakerState === "CLOSED", "Circuit breaker state is CLOSED");

    // 3. Flush via alarm() simulation
    await telemetryDO.alarm();
    assert(telemetryDO.getQueue().getStats().bufferSize === 0, "Buffer empty after DO alarm flush");
    assert(telemetryDO.getQueue().getStats().totalExported === 3, "Total exported is 3");

    // 4. Drain endpoint
    telemetryDO.getQueue().enqueue(generateTestSpan(42));
    const drainReq = new Request("https://edge.hagibis.internal/api/telemetry/drain", { method: "POST" });
    const drainRes = await telemetryDO.fetch(drainReq);
    assert(drainRes.status === 200, "Drain endpoint returned 200 OK");
    const drainBody: any = await drainRes.json();
    assert(drainBody.count === 1, "Drained 1 span");
    assert(drainBody.spans[0].name === "test_span_42", "Drained correct span");
    assert(telemetryDO.getQueue().getStats().bufferSize === 0, "Buffer empty after drain");

    passedTests++;
  }

  // ---------------------------------------------------------------------------
  // SUITE 6: Turnkey Route Middleware (`withEdgeTracing`) Zero-Latency Decoupling
  // ---------------------------------------------------------------------------
  totalTests++;
  console.log("\nTEST 6: withEdgeTracing Middleware Decoupling & ctx.waitUntil Verification");
  {
    resetGlobalTelemetryQueue();
    const queue = getGlobalTelemetryQueue({ maxBatchSize: 50 });

    let bgPromiseReceived: Promise<any> | null = null;
    const mockCtx: ExecutionContextLike = {
      waitUntil: (p) => {
        bgPromiseReceived = p;
      },
    };

    const mockEnv: EdgeTelemetryEnv = {
      HAGIBIS_EDGE_KV: {
        put: async () => {},
        get: async () => null,
      },
    };

    const testReq = new Request("https://api.hagibis.edge/api/edge/wasm/execute", {
      method: "POST",
      headers: {
        "user-agent": "hagibis-runner/1.0",
        "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
      },
    });

    const startExecution = performance.now();
    const response = await withEdgeTracing(
      testReq,
      mockEnv,
      mockCtx,
      "wasi_edge_execute_test",
      async (span: EdgeSpan) => {
        span.setAttribute("app.tool", "matrix_mult");
        span.addEvent("compilation_finished");
        return new Response(JSON.stringify({ output: "computed" }), {
          status: 200,
          headers: { "Content-Type": "application/json" },
        });
      }
    );
    const edgeHandlerDurationMs = performance.now() - startExecution;

    console.log(`  withEdgeTracing route execution time: ${edgeHandlerDurationMs.toFixed(3)}ms`);
    assert(response.status === 200, "Response status is 200 OK");
    assert(response.headers.has("traceparent"), "Response contains W3C traceparent header");
    assert(response.headers.has("x-trace-id"), "Response contains x-trace-id header");
    assert(response.headers.get("x-trace-id") === "4bf92f3577b34da6a3ce929d0e0e4736", "Trace ID propagated accurately");
    assert(queue.getStats().bufferSize === 1, "Completed span enqueued in global telemetry queue");

    // Trigger batch export via force flush
    queue.scheduleFlush(mockEnv, mockCtx, true);
    assert(bgPromiseReceived !== null, "Background flush scheduled via ctx.waitUntil");
    await bgPromiseReceived;

    assert(queue.getStats().bufferSize === 0, "Global queue drained after background flush");
    assert(queue.getStats().totalExported === 1, "Global queue recorded 1 exported span");

    passedTests++;
  }

  console.log("\n================================================================================");
  console.log(`ALL VERIFICATION SUITES PASSED! (${passedTests}/${totalTests} tests)`);
  console.log("Pillar 4: Asynchronous Non-Blocking Telemetry Pipelines 100% OPERATIONAL.");
  console.log("================================================================================");
}

runTestSuite().catch((err) => {
  console.error("FATAL: Test suite failed with error:", err);
  process.exit(1);
});
