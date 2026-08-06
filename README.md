# NylonMem — A Memory Engine for AI Agents

[简体中文](README.zh-CN.md) | English

> Single-node memory engine for AI agents: multi-filament memory weaving, mesh memory graph, tension-based forgetting, context resonance.
> Status: early development (Phase 1, APIs are not yet stable)

Nylon models each memory as a braid of multiple "filaments" (fact / emotion / temporal / relation / confidence / frequency), with memory nodes connected by weighted edges in a mesh graph rather than a hierarchy. Retrieval is not top-k similarity matching but **context resonance**: starting from seed nodes, activation spreads across the graph proportional to edge strength x context match x real-time tension, bounded by a global activation budget (so high-fan-out nodes cannot explode the traversal). Unused memories decay along a tension forgetting curve instead of piling up forever.

## Quick Start

```bash
cd engine
cargo test                    # run all unit tests
cargo run -p nylon-engine     # run the self-check demo
```

Demo output (seed = "flight tickets", task context = "business trip"):

```
context resonance (seed=flight, task=trip):
  node 0: resonance=0.919  user asked about flights
  node 1: resonance=0.627  trip preference: window seat
  node 2: resonance=0.310  hotel preference: near subway
  node 3: resonance=0.247  last trip: Shanghai, 2026-06
```

## Repository Layout

```
nylon/
├── proto/            # nylon/v1 gRPC contract (Weave / Resonate / Search / GetNode)
└── engine/           # Rust workspace
    └── crates/
        ├── nylon-core    # filament data model + tension forgetting (logistic-normalized)
        ├── nylon-graph   # CSR main graph + delta buffer + resonance traversal
        ├── nylon-vector  # vector index abstraction (brute-force cosine baseline, HNSW WIP)
        └── nylon-engine  # engine entrypoint (gRPC serving in progress)
```

## Status & Roadmap

Done: core data model, CSR + delta incremental graph, resonance traversal, vector baseline.
In progress: HNSW index, gRPC serving (tonic), RocksDB persistence, Python SDK.

## Contributing

Contributions are welcome! Please read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a PR.
Note: this project requires a CLA (Contributor License Agreement); a bot will guide you through signing on your first PR.

## License

[Apache License 2.0](LICENSE). "Nylon" and the Nylon logo are trademarks of the project; the license does not grant trademark rights (see [NOTICE](NOTICE)).