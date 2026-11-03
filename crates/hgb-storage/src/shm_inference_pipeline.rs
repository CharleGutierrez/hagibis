//! # True Zero-Copy Shared Memory for AI Inference Pipelines (Storage Engine)
//!
//! Re-exports the unified zero-copy tensor streaming and WebAssembly direct memory bridging pipeline
//! from `hgb-core`, providing high-throughput POSIX shared memory ring buffers for local LLMs,
//! KV cache tensor streams, and WASM guest linear memory integration.
//!
//! Provides length-prefixed atomic framing with 64-byte cacheline-aligned binary headers,
//! lock-free head/tail ring navigation with magic `0x4847425F494E4631` ("HGB_INF1"),
//! sub-millisecond IPC streaming, and deterministic memory isolation verification.

pub use hgb_core::shm_inference_pipeline::{
    resolve_infer_stream_path, stream_ollama_embedding_to_wasm, InferControlHeader, InferDataType, TensorFrameHeader,
    TensorStreamBenchReport, TensorStreamStatus, WasmDirectMemoryBridge,
    ZeroCopyTensorConsumer, ZeroCopyTensorProducer, ZeroCopyTensorStream,
    DEFAULT_INFER_STREAM_CAPACITY, FLAG_CHECKSUMMED, FLAG_INTERMEDIATE_ACTIVATION,
    FLAG_IS_FINAL, FLAG_KV_CACHE, INFER_CONTROL_HEADER_SIZE, INFER_FRAME_HEADER_SIZE,
    INFER_FRAME_MAGIC, INFER_STREAM_VERSION,
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    use hgb_core::wasmtime::{Config, Engine, Memory, MemoryType, OptLevel, Store};

    #[test]
    fn test_tensor_stream_initialization_and_metadata() {
        let stream_id = format!("test_init_{}", std::process::id());
        let mut stream = ZeroCopyTensorStream::create(&stream_id, 1024 * 1024).expect("create stream");
        stream.set_unlink_on_drop(true);

        assert_eq!(stream.stream_id(), stream_id);
        assert!(stream.capacity() >= 1024 * 1024);

        let status = stream.status();
        assert_eq!(status.stream_id, stream_id);
        assert!(status.is_empty);
        assert!(!status.is_full);
        assert_eq!(status.unread_bytes, 0);
        assert_eq!(status.total_frames_sequenced, 0);
    }

    #[test]
    fn test_tensor_stream_atomic_write_read_roundtrip() {
        let stream_id = format!("test_atomic_rw_{}", std::process::id());
        let mut stream = ZeroCopyTensorStream::create(&stream_id, 512 * 1024).expect("create stream");
        stream.set_unlink_on_drop(true);

        // 1. Write token stream chunk
        let tokens = "fn compute_attention(q: &[f32], k: &[f32]) -> f32 {";
        let written = stream.write_tokens(tokens, false, 0).expect("write tokens");
        assert!(written);

        // 2. Read back token chunk
        let (recvd_tokens, is_final, token_idx) = stream.read_tokens().expect("read tokens").expect("some tokens");
        assert_eq!(recvd_tokens, tokens);
        assert!(!is_final);
        assert_eq!(token_idx, 0);

        // 3. Write f32 tensor (e.g. prompt embedding or activation layer)
        let dims = [1, 4, 8];
        let tensor_data: Vec<f32> = (0..32).map(|i| (i as f32) * 0.1).collect();
        let written_tensor = stream.write_f32_tensor(&dims, &tensor_data).expect("write f32 tensor");
        assert!(written_tensor);

        // 4. Read back f32 tensor
        let (recvd_dims, recvd_data) = stream.read_f32_tensor().expect("read f32 tensor").expect("some tensor");
        assert_eq!(recvd_dims, vec![1, 4, 8]);
        assert_eq!(recvd_data.len(), 32);
        for (a, b) in tensor_data.iter().zip(recvd_data.iter()) {
            assert!((a - b).abs() < 1e-6);
        }

        // 5. Write Q8_0 intermediate KV cache representation
        let kv_dims = [1, 16, 64];
        let kv_bytes: Vec<u8> = (0..1024).map(|i| (i % 256) as u8).collect();
        assert!(stream.write_kv_cache(&kv_dims, &kv_bytes).expect("write kv cache"));

        // 6. Read back KV cache
        let (recvd_kv_dims, recvd_kv_bytes) = stream.read_kv_cache().expect("read kv cache").expect("some kv cache");
        assert_eq!(recvd_kv_dims, vec![1, 16, 64]);
        assert_eq!(recvd_kv_bytes, kv_bytes);
    }

    #[test]
    fn test_tensor_stream_circular_wrapping() {
        // Use a small buffer (64 KiB) to force multiple wrap-around cycles
        let stream_id = format!("test_wrap_{}", std::process::id());
        let cap = 64 * 1024;
        let mut stream = ZeroCopyTensorStream::create(&stream_id, cap).expect("create stream");
        stream.set_unlink_on_drop(true);

        let chunk = vec![0x3Cu8; 4096];
        let dims = [4096u64];
        let iterations = 40; // Total 160 KiB transferred through 64 KiB buffer

        for i in 0..iterations {
            let written = stream.write_tensor_frame(InferDataType::Utf8Tokens, &dims, &chunk).expect("write frame");
            assert!(written, "Failed write at iteration {}", i);

            let (header, payload) = stream.read_frame().expect("read frame").expect("some frame");
            assert_eq!(header.payload_len as usize, 4096);
            assert_eq!(payload.len(), 4096);
            assert_eq!(payload[0], 0x3C);
            assert_eq!(payload[4095], 0x3C);
        }

        // Test zero-copy wrapping reading across the buffer boundary
        let zc_written = stream.write_tokens("wrapping_zero_copy_test", true, 99).expect("write tokens");
        assert!(zc_written);

        let zc_res = stream.read_zero_copy(|hdr, s1, s2| {
            assert_eq!(hdr.data_type, InferDataType::Utf8Tokens as u32);
            let mut combined = s1.to_vec();
            combined.extend_from_slice(s2);
            combined
        }).expect("read zero copy").expect("some zero copy");

        assert_eq!(zc_res, b"wrapping_zero_copy_test");
    }

    #[test]
    fn test_token_stream_serialization_benchmark_sub_millisecond() {
        let stream_id = format!("test_bench_{}", std::process::id());
        let mut stream = ZeroCopyTensorStream::create(&stream_id, 2 * 1024 * 1024).expect("create stream");
        stream.set_unlink_on_drop(true);

        let iterations = 1000;
        let chunk_size = 512; // Typical LLM token packet
        let report = stream.benchmark(chunk_size, iterations).expect("run benchmark");

        println!(
            "Sub-millisecond Token Stream Serialization Benchmark:\n  Throughput: {:.2} MB/s\n  Avg Latency: {:.2} ns ({:.4} ms)\n  Sub-ms Verified: {}\n  Status: {}",
            report.throughput_mb_s, report.avg_latency_ns, report.avg_latency_ns / 1_000_000.0,
            report.sub_millisecond_verified, report.status
        );

        assert!(report.sub_millisecond_verified, "Latency was {} ns, expected < 1,000,000 ns", report.avg_latency_ns);
        assert!(report.avg_latency_ns < 100_000.0, "IPC streaming latency too high: {} ns", report.avg_latency_ns);
        assert!(report.zero_copy_verified);
    }

    #[test]
    fn test_wasm_direct_memory_bridge_simulated_linear_memory() {
        let stream_id = format!("test_wasm_bridge_{}", std::process::id());
        let mut stream = ZeroCopyTensorStream::create(&stream_id, 512 * 1024).expect("create stream");
        stream.set_unlink_on_drop(true);

        // Simulated WASM guest linear memory page (64 KiB)
        let mut guest_memory = vec![0u8; 65536];

        // 1. Write tensor frame into shared memory
        let tensor_data: Vec<f32> = vec![1.5, 2.5, 3.5, 4.5, 5.5, 6.5, 7.5, 8.5];
        let dims = [1, 8];
        assert!(stream.write_f32_tensor(&dims, &tensor_data).expect("write f32"));

        // 2. Bridge directly into guest linear memory at offset 1024 (4-byte aligned)
        let guest_offset = 1024;
        let (header, bytes_bridged) = WasmDirectMemoryBridge::bridge_stream_into_guest(
            &mut stream,
            &mut guest_memory,
            guest_offset,
        ).expect("bridge into guest").expect("some bridged frame");

        assert_eq!(header.data_type, InferDataType::F32 as u32);
        assert_eq!(bytes_bridged, 32); // 8 * 4 bytes

        // Verify guest memory has the exact floats without intermediate copy
        let guest_slice = &guest_memory[guest_offset..guest_offset + bytes_bridged];
        let mut recvd_floats = Vec::new();
        for chunk in guest_slice.chunks_exact(4) {
            recvd_floats.push(f32::from_le_bytes(chunk.try_into().unwrap()));
        }
        assert_eq!(recvd_floats, tensor_data);

        // 3. Bridge back from guest linear memory into stream
        let new_offset = 2048;
        guest_memory[new_offset..new_offset + 14].copy_from_slice(b"WASM_RESPONSE!");
        assert!(WasmDirectMemoryBridge::bridge_guest_into_stream(
            &mut stream,
            &guest_memory,
            new_offset,
            14,
            InferDataType::Utf8Tokens,
            &[14],
        ).expect("bridge guest into stream"));

        let (hdr, payload) = stream.read_frame().expect("read frame").expect("some frame");
        assert_eq!(hdr.data_type, InferDataType::Utf8Tokens as u32);
        assert_eq!(&payload, b"WASM_RESPONSE!");
    }

    #[test]
    fn test_wasm_direct_memory_bridge_wasmtime_instance() {
        let stream_id = format!("test_wasmtime_{}", std::process::id());
        let mut stream = ZeroCopyTensorStream::create(&stream_id, 512 * 1024).expect("create stream");
        stream.set_unlink_on_drop(true);

        // Real Wasmtime engine and store
        let mut config = Config::new();
        config.cranelift_opt_level(OptLevel::Speed);
        let engine = Engine::new(&config).expect("wasmtime engine");
        let mut store = Store::new(&engine, ());

        // Create 2 pages (128 KiB) of linear memory
        let mem_type = MemoryType::new(2, Some(4));
        let memory = Memory::new(&mut store, mem_type).expect("wasmtime memory");

        // Write token stream
        let tokens = "Direct Wasmtime Linear Memory Bridge!";
        assert!(stream.write_tokens(tokens, true, 42).expect("write tokens"));

        // Bridge directly into Wasmtime memory at offset 256
        let guest_offset = 256;
        let (header, bridged_len) = WasmDirectMemoryBridge::bridge_stream_into_wasmtime(
            &mut stream,
            &mut store,
            &memory,
            guest_offset,
        ).expect("bridge wasmtime").expect("some frame");

        assert_eq!(header.token_index, 42);
        assert_eq!(bridged_len, tokens.len());

        // Verify data inside Wasmtime linear memory
        let data = memory.data(&store);
        assert_eq!(&data[guest_offset..guest_offset + tokens.len()], tokens.as_bytes());

        // Bridge back from Wasmtime memory
        let echo_offset = 512;
        memory.data_mut(&mut store)[echo_offset..echo_offset + 5].copy_from_slice(b"ECHO!");
        assert!(WasmDirectMemoryBridge::bridge_wasmtime_into_stream(
            &mut stream,
            &store,
            &memory,
            echo_offset,
            5,
            InferDataType::Utf8Tokens,
            &[5],
        ).expect("bridge wasmtime into stream"));

        let (echo_text, is_final, _) = stream.read_tokens().expect("read tokens").expect("some tokens");
        assert_eq!(echo_text, "ECHO!");
        assert!(!is_final);
    }

    #[test]
    fn test_tamper_detection_and_buffer_overflow_protections() {
        let stream_id = format!("test_security_{}", std::process::id());
        let mut stream = ZeroCopyTensorStream::create(&stream_id, 128 * 1024).expect("create stream");
        stream.set_unlink_on_drop(true);

        // 1. Buffer overflow protection in isolation validation
        let small_guest_mem = vec![0u8; 128];
        let overflow_err = WasmDirectMemoryBridge::validate_isolation_bounds(
            small_guest_mem.len(),
            100,
            64, // 100 + 64 = 164 > 128
            InferDataType::Utf8Tokens,
        );
        assert!(overflow_err.is_err());
        assert!(overflow_err.unwrap_err().to_string().contains("Memory isolation violation"));

        // 2. Alignment invariant check (F32 requires 4-byte alignment)
        let misaligned_err = WasmDirectMemoryBridge::validate_isolation_bounds(
            small_guest_mem.len(),
            3, // Not 4-byte aligned
            16,
            InferDataType::F32,
        );
        assert!(misaligned_err.is_err());
        assert!(misaligned_err.unwrap_err().to_string().contains("Memory alignment invariant violation"));

        // 3. Oversized payload exceeding total ring capacity
        let oversized = vec![0u8; 200 * 1024]; // 200 KiB > 128 KiB capacity
        let cap_err = stream.write_tensor_frame(InferDataType::Utf8Tokens, &[200 * 1024], &oversized);
        assert!(cap_err.is_err());
        assert!(cap_err.unwrap_err().to_string().contains("exceeds total tensor stream capacity"));

        // 4. Tamper detection: flip byte in mmap payload to trigger checksum mismatch
        let original_data = b"Cryptographically protected tensor payload";
        assert!(stream.write_tokens(std::str::from_utf8(original_data).unwrap(), false, 1).expect("write tokens"));

        // Corrupt payload byte directly in shared memory mmap
        let payload_offset = INFER_CONTROL_HEADER_SIZE + INFER_FRAME_HEADER_SIZE;
        stream.as_mut_slice()[payload_offset + 5] ^= 0xFF; // Tamper!

        // Must fail with cryptographic verification checksum mismatch
        let read_result = stream.read_frame();
        assert!(read_result.is_err());
        let err_msg = read_result.unwrap_err().to_string();
        assert!(err_msg.contains("Checksum mismatch"));
    }

    #[test]
    fn test_tensor_producer_consumer_split() {
        let stream_id = format!("test_split_{}", std::process::id());
        let stream = ZeroCopyTensorStream::create(&stream_id, 256 * 1024).expect("create stream");
        let (mut producer, mut consumer) = stream.split().expect("split stream");
        producer.set_unlink_on_drop(true);

        let count = 50;
        for i in 0..count {
            let text = format!("token_chunk_{}", i);
            assert!(producer.write_tokens(&text, i == count - 1, i as u64).expect("write"));
        }

        for i in 0..count {
            let (recvd, is_final, idx) = consumer.read_frame().expect("read").map(|(hdr, p)| {
                (String::from_utf8(p).unwrap(), (hdr.flags & FLAG_IS_FINAL) != 0, hdr.token_index)
            }).expect("some token");

            assert_eq!(recvd, format!("token_chunk_{}", i));
            assert_eq!(idx, i as u64);
            assert_eq!(is_final, i == count - 1);
        }
    }
}
