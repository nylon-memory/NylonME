# NylonME — A Memory Engine for AI Agents

[简体中文](README.zh-CN.md) | English

> Single-node memory engine for AI agents: multi-filament memory weaving, mesh memory graph, tension-based forgetting, context resonance.
> Status: Phase 2 — active development (APIs may still evolve)

NylonME models each memory as a braid of multiple "filaments" (fact / emotion / temporal / relation / confidence / frequency), with memory nodes connected by weighted edges in a mesh graph rather than a hierarchy. Retrieval is not top-k similarity matching but **context resonance**: starting from seed nodes, activation spreads across the graph proportional to edge strength x context match x real-time tension, bounded by a global activation budget (so high-fan-out nodes cannot explode the traversal). Unused memories decay along a tension forgetting curve instead of piling up forever.

## Benchmark

LoCoMo evidence recall@10, full 10-session corpus (1536 answerable QA, lexical + vector hybrid retrieval):

| Stage | recall@10 |
|---|---|
| Lexical baseline | 47.1% |
| + vector seeds (bge-m3) + graph | 70.6% |
| + dual-layer write (leaf turns + session-level LLM facts) | 79.2% |
| + adaptive resonance depth (Cat4 single-hop queries skip diffusion) | 80.1% |
| + query-vector rerank of the activated set | **84.6%** |
| + async commonsense reflection (world-knowledge bridges) | **85.3%** |

Per-category (full corpus): multi-hop 84.8%, temporal 86.6%, commonsense 60.9%, single-hop 87.6%.

Two design rules the experiments forced on us: the **understanding layer lives on the write side** (the LLM is a compiler that turns raw events into retrievable structure; query-side LLM expansion measured net-zero), and **both layers must coexist** (abstract-layer-only retrieval drops the score to 67.3%).


## Use from Your Agent in 2 Minutes (MCP)

`nylon-engine mcp` speaks the Model Context Protocol over stdio with the engine **embedded** — no daemon, no ports; data lands in `~/.nylonme/data` automatically. Download a binary from [Releases](https://github.com/nylon-memory/NylonME/releases) and point your MCP client at it:

```json
{
  "mcpServers": {
    "nylonme": {
      "command": "/path/to/nylon-engine",
      "args": ["mcp"],
      "env": { "NYLON_OWNER": "my-project" }
    }
  }
}
```

Works with Claude Code, Cursor, Codex, VS Code Copilot and any MCP client. Your agent gets three tools: `memory_weave` (persist a fact), `memory_resonate` (recall related memories), `memory_get` (read a node). See [docs/GETTING_STARTED.md](docs/GETTING_STARTED.md) for per-client setup.

## Web Console (built-in)

The engine binary also serves a zero-install web console and REST API (default `http://127.0.0.1:50052`, `NYLON_HTTP_ADDR=off` disables):

![NylonME Console](docs/ui-console.png)

Browse memories with real-time tension, debug resonance queries (seeds, scores, adaptive depth), and weave new memories by hand — same engine, same write path as gRPC/MCP. REST endpoints mirror the gRPC contract; spec: [docs/api/openapi.json](docs/api/openapi.json).

## Python SDK

`nylon-sdk` wraps the gRPC contract in sync + async clients:

```bash
pip install ./sdk/python    # from this repo
```

```python
from nylon_sdk import NylonClient

with NylonClient("127.0.0.1:50051", owner="alice") as client:
    client.weave("Alice prefers window seats on business trips")
    for node in client.resonate("flight seat preference").activated:
        print(node.filaments.fact)
```

See [sdk/python/README.md](sdk/python/README.md).
## Quick Start

```bash
cd engine
cargo test                    # run all unit tests
cargo run -p nylon-engine     # run the self-check demo
```

Serve as a gRPC daemon (RocksDB-backed persistence):

```bash
NYLON_DATA_DIR=./data \
NYLON_EMBED_URL=http://localhost:11434 NYLON_EMBED_MODEL=bge-m3 NYLON_EMBED_DIMS=1024 \
cargo run --release -p nylon-engine -- serve 0.0.0.0:50051
```

Optional understanding layer (session-level fact weaving; any OpenAI-compatible chat endpoint):

```bash
NYLON_LLM_URL=https://api.deepseek.com/v1/chat/completions \
NYLON_LLM_MODEL=deepseek-v4-flash NYLON_LLM_API_KEY=... NYLON_LLM_THINKING_OFF=1 ...
```

A small CLI is included for manual operations:

```bash
cargo run --release --example nylon_cli -- resonate --owner alice --query "when did we book the flights" --budget 8
cargo run --release --example nylon_cli -- weave --owner alice --fact "Alice prefers window seats"
```

## Tuning Knobs (env vars)

| Variable | Default | Effect |
|---|---|---|
| `NYLON_MAX_SEEDS` | 20 | seed set size cap (lexical + vector channels) |
| `NYLON_CAT{n}_MAX_HOPS` | — | per-query-type diffusion depth override (`0` = seeds only, no spread) |
| `NYLON_RERANK_VEC` | 0 | blend weight of query-node cosine similarity into resonance ranking |
| `NYLON_TENSION_FLOOR` | 0 | lower bound on tension during ranking (does not mutate node state) |
| `NYLON_SEED_QUOTA` | 0 | reserved front slots for direct-match seeds in the output |
| `NYLON_DERIVED_EDGES` | off | explicit abstract-layer → leaf edges (measured net-negative for temporal/commonsense, keep off) |

## Repository Layout

```
nylon/
├── proto/            # nylon/v1 gRPC contract (Weave / WeaveSession / Resonate / Search / GetNode)
└── engine/           # Rust workspace
    └── crates/
        ├── nylon-core    # filament data model + tension forgetting
        ├── nylon-graph   # CSR main graph + delta buffer + resonance traversal
        ├── nylon-vector  # HNSW vector index
        ├── nylon-embed   # embedding client (ollama / OpenAI-compatible)
        ├── nylon-llm     # understanding layer: fact weaving, conflict detection
        ├── nylon-storage # RocksDB persistence (WAL + snapshot, crash recovery)
        └── nylon-engine  # engine entrypoint + gRPC service (tonic)
```

## Roadmap

Done: dual-layer write engine (WeaveSession), hybrid lexical+vector seeds, adaptive resonance depth, query-vector rerank, HNSW, RocksDB persistence, gRPC serving, LoCoMo 84.6%.
Next: commonsense-reasoning retrieval path (Cat3, currently 53.3%), cross-encoder reranker, Python SDK, 1M-node memory profiling, paper & blog series.

## Contributing

Contributions are welcome! Please read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a PR.
Note: this project requires a CLA (Contributor License Agreement); a bot will guide you through signing on your first PR.

## License

[Apache License 2.0](LICENSE). "NylonME" and the NylonME logo are trademarks of the project; the license does not grant trademark rights (see [NOTICE](NOTICE)).