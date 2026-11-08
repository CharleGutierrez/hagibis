---
name: vector-lancedb
description: Embedded LanceDB-compatible vector storage and semantic querying for Hagibis
version: 0.1.0
category: storage
---

# Embedded LanceDB Vector Storage & Semantic Search Engine

`vector-lancedb` provides production-grade, serverless, zero-copy embedded vector storage and semantic querying for Hagibis. Inspired by the Lance columnar format and LanceDB architecture, this storage engine is optimized for high-throughput AI agent operations, local SLM/LLM semantic memory retrieval, and low-latency code symbol indexing.

## 1. Architectural Highlights

- **Serverless & In-Process**: Runs directly within the Hagibis runtime without background daemon dependencies or external network sockets.
- **Lance Columnar Disk Format**: Separates dense contiguous vector arrays (`.vec`) from tabular text and JSON metadata columns (`.col`) for maximum cache efficiency and disk I/O minimization.
- **Zero-Copy Memory-Mapped Retrieval**: Exploits `memmap2` to map binary float vectors directly into virtual memory pages. Scanning thousands of vectors requires zero heap allocations.
- **SIMD Distance Acceleration**: 8-way unrolled vector kernels leveraging CPU vector units (AVX2, FMA, ARM Neon) for Cosine similarity, L2 Euclidean distance, and Dot product computations.
- **Atomic Manifest & Commit Logging**: Multi-Version Concurrency Control (MVCC) with append-only commit logs (`_commits/`) and version manifests (`_versions/`), guaranteeing snapshot isolation and crash resilience.
- **Hybrid Search Fusion**: Blends dense semantic vector embeddings with fast BM25 lexical token matching via configurable weighted linear interpolation (`hybrid_alpha`) or Reciprocal Rank Fusion (RRF).
- **Multi-Partition IVF Indexing**: Centroid-based Inverted File (IVF) clustering allowing sub-linear multi-partition search and Voronoi cell pruning.
- **Columnar Predicate Filtering**: Evaluates metadata expressions (`==`, `!=`, `<`, `<=`, `>`, `>=`, `contains`, `in`) directly against structured records.

---

## 2. Lance Columnar Disk Layout

The `.lance` dataset directory adheres to the following structure:

```
<dataset_path>.lance/
├── _latest_version           # Plaintext integer pointer to active manifest version
├── _versions/
│   ├── 0000000001.manifest   # JSON metadata manifest for version 1
│   └── 0000000002.manifest   # JSON metadata manifest for version 2
├── _commits/
│   ├── commit_0000000001.log # Atomic transaction commit log
│   └── commit_0000000002.log # Subsequent upsert/delete transaction
└── data/
    ├── fragment_0000000001.vec # 64-byte aligned contiguous f32 vector file (mmap)
    ├── fragment_0000000001.col # Columnar string/JSON/offset table file
    ├── fragment_0000000002.vec
    └── fragment_0000000002.col
```

### 2.1 Aligned Binary Vector File (`.vec`)

The `.vec` file begins with a 64-byte cacheline-aligned binary header:

| Offset | Field | Type | Description |
|---|---|---|---|
| 0x00 | `magic` | `u64` | `0x4C414E4345564543` ("LANCEVEC") |
| 0x08 | `version` | `u32` | Format version (`1`) |
| 0x0C | `dimension` | `u32` | Vector dimensionality ($D$, e.g. 384, 768, 1536) |
| 0x10 | `count` | `u64` | Total records in this fragment |
| 0x18 | `metric` | `u32` | Default distance metric (0 = Cosine, 1 = L2, 2 = Dot) |
| 0x1C | `reserved` | `[u8; 36]` | 64-byte boundary padding |

Immediately following offset 64 (`0x40`), dense float vectors are stored contiguously in IEEE-754 32-bit floating point format:
$$\text{Vector}(i) = \left[ \text{vec}[i \cdot D], \text{vec}[i \cdot D + 1], \dots, \text{vec}[(i+1) \cdot D - 1] \right]$$

Because offset 64 is a multiple of 64 bytes (and 4 bytes), direct memory-mapped slices `&[f32]` can be safely referenced with zero copying or deserialization.

### 2.2 Columnar Data File (`.col`)

Tabular text, timestamps, partition names, and JSON metadata are stored in `.col` files with an O(1) index offset table at the tail:
- **Header**: Magic `0x4C414E4345434F4C` ("LANCECOL"), version, and entry count.
- **Record Entries**: Length-prefixed string ID, partition, timestamp (`u64`), length-prefixed raw text, and length-prefixed JSON metadata bytes.
- **Offset Index**: An array of `(offset: u64, length: u32)` tuples allowing random access to only the metadata of the Top-K surviving vector matches.

---

## 3. SIMD Distance Kernel Mathematics

### Cosine Similarity
$$\text{sim}(\mathbf{u}, \mathbf{v}) = \frac{\sum_{i=0}^{D-1} u_i v_i}{\sqrt{\sum_{i=0}^{D-1} u_i^2} \sqrt{\sum_{i=0}^{D-1} v_i^2}}$$
- Distance: $1.0 - \text{sim}(\mathbf{u}, \mathbf{v})$ (in $[0, 2]$).
- Score: $\text{sim}(\mathbf{u}, \mathbf{v})$ (in $[-1, 1]$).

### Euclidean (L2) Distance
$$d_{L2}(\mathbf{u}, \mathbf{v}) = \sqrt{\sum_{i=0}^{D-1} (u_i - v_i)^2}$$
- Distance: $d_{L2}(\mathbf{u}, \mathbf{v})$.
- Normalized Score: $\frac{1.0}{1.0 + d_{L2}(\mathbf{u}, \mathbf{v})}$ (in $(0, 1]$).

### Dot Product
$$\text{dot}(\mathbf{u}, \mathbf{v}) = \sum_{i=0}^{D-1} u_i v_i$$
- Distance: $-\text{dot}(\mathbf{u}, \mathbf{v})$.
- Score: $\text{dot}(\mathbf{u}, \mathbf{v})$.

---

## 4. Hybrid Search Fusion

When a query specifies both a dense query vector $\mathbf{q}$ and an optional lexical query string $T_q$:
1. **Dense Vector Scoring**: Evaluates SIMD similarity $S_{\text{vec}} \in [0, 1]$ across all candidate records.
2. **Lexical BM25 Scoring**: Computes Okapi BM25 scores:
   $$\text{BM25}(D, Q) = \sum_{t \in Q} \text{IDF}(t) \cdot \frac{f(t, D) \cdot (k_1 + 1)}{f(t, D) + k_1 \cdot \left(1 - b + b \cdot \frac{|D|}{\text{avgdl}}\right)}$$
   where $k_1 = 1.2$, $b = 0.75$, and $\text{IDF}(t) = \ln\left(1 + \frac{N - n(t) + 0.5}{n(t) + 0.5}\right)$.
3. **Linear Fusion**:
   $$S_{\text{hybrid}} = \alpha \cdot S_{\text{vec}} + (1 - \alpha) \cdot S_{\text{lex}}$$
   where $\alpha \in [0.0, 1.0]$ defaults to $0.7$.

---

## 5. Multi-Partition Indexing & Centroid/IVF Clustering

Records can be segmented by partition tags (e.g. `"codebase"`, `"interaction"`, `"docs"`). For each partition $P_k$, the engine maintains a dynamically updated centroid $\mathbf{c}_k$:
$$\mathbf{c}_k = \frac{1}{|P_k|} \sum_{\mathbf{v} \in P_k} \mathbf{v}$$

During queries:
- **Explicit Partitioning**: If `query.partition = Some("codebase")`, only fragments belonging to that partition are scanned.
- **Centroid IVF Pruning**: If `query.partition = None` and `nprobes` is specified, the engine calculates the distance between $\mathbf{q}$ and each partition centroid $\mathbf{c}_k$, probing only the nearest $n$ partitions to prune search spaces on large datasets.

---

## 6. Rust API Usage

```rust
use hgb_storage::lancedb_vector_engine::{
    DistanceMetric, LanceDbVectorEngine, SearchQuery, VectorRecord,
};
use serde_json::json;

// 1. Initialize or open an existing Lance dataset
let engine = LanceDbVectorEngine::open_or_create("/path/to/my_data.lance", 4, DistanceMetric::Cosine)?;

// 2. Upsert vector records
let records = vec![
    VectorRecord {
        id: "doc-1".to_string(),
        vector: vec![0.1, 0.2, 0.3, 0.4],
        text: "Zero-copy memory mapped vector search with LanceDB".to_string(),
        metadata: json!({"category": "storage", "tier": 2}),
        timestamp: 1731000000,
        partition: "default".to_string(),
    },
    VectorRecord {
        id: "doc-2".to_string(),
        vector: vec![0.9, 0.8, 0.7, 0.6],
        text: "WebAssembly microkernel execution and sandboxing".to_string(),
        metadata: json!({"category": "wasm", "tier": 1}),
        timestamp: 1731000001,
        partition: "wasm".to_string(),
    },
];
engine.upsert(records)?;

// 3. Query with hybrid vector + lexical search and metadata filtering
let query = SearchQuery {
    vector: vec![0.1, 0.2, 0.3, 0.4],
    text_query: Some("memory mapped search".to_string()),
    top_k: 5,
    metric: DistanceMetric::Cosine,
    filter: Some("category == 'storage'".to_string()),
    partition: None,
    hybrid_alpha: Some(0.7),
    nprobes: None,
};
let results = engine.query(&query)?;

for r in results {
    println!("Match: id={}, score={}, text={}", r.id, r.score, r.text);
}
```

---

## 7. Model Context Protocol (MCP) Tool Integration

Two tools are exported for the agentic mesh:

### `hgb_vector_lancedb_query`
Executes zero-copy vector and hybrid semantic queries across the LanceDB dataset.
- `vector` (`array[number]`): Query embedding vector.
- `text_query` (`string`, optional): Natural language or keyword query for BM25 hybrid ranking.
- `top_k` (`integer`): Number of nearest neighbors (default 10).
- `metric` (`enum: ["cosine", "l2", "dot"]`): Distance metric.
- `filter` (`string`, optional): Metadata predicate expression (e.g. `category == 'code'`).
- `partition` (`string`, optional): Filter to specific partition.
- `hybrid_alpha` (`number`): Balance between vector and lexical ranking (0.0 to 1.0).
- `table_path` (`string`, optional): Target `.lance` dataset path.

### `hgb_vector_lancedb_upsert`
Atomically appends or updates vector records with automatic manifest generation.
- `records` (`array[object]`): List of records with `id`, `vector`, `text`, `metadata`, `timestamp`, `partition`.
- `table_path` (`string`, optional): Target `.lance` dataset path.
