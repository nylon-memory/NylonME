# Getting Started with NylonME

[English] | 这篇文档带你 5 分钟跑起 NylonME：下载预编译二进制或从源码构建，启动引擎，写入并召回第一条记忆。

## Option A: Download a prebuilt release (fastest)

Grab the latest archive for your platform from [GitHub Releases](https://github.com/nylon-memory/NylonME/releases):

| Asset | Platform |
|---|---|
| `nylonme-linux-x64.tar.gz` | Linux x86_64 |
| `nylonme-windows-x64.zip` | Windows x86_64 |
| `nylonme-macos-arm64.tar.gz` | Apple Silicon |

Each archive contains:

- `nylon-engine` — the gRPC memory engine daemon
- `nylon_cli` — a small CLI for weaving and resonating memories
- `smoke_client` — a 4-RPC end-to-end smoke test

## Option B: Build from source

Prerequisites: Rust (stable), `protoc`, and on Linux also `clang` + `libclang-dev` (needed by the RocksDB bindings).

```bash
# Debian/Ubuntu
sudo apt install -y protobuf-compiler clang libclang-dev pkg-config libssl-dev
# macOS
brew install protobuf

git clone https://github.com/nylon-memory/NylonME.git
cd NylonME/engine
cargo build --release -p nylon-engine --example nylon_cli
```

## Run the engine

Minimal (in-memory embeddings off, LLM off):

```bash
NYLON_DATA_DIR=./data ./nylon-engine serve 0.0.0.0:50051
```

Recommended (semantic recall via an ollama embedding model):

```bash
ollama pull bge-m3   # any OpenAI-compatible / ollama endpoint works
NYLON_DATA_DIR=./data \
NYLON_EMBED_URL=http://localhost:11434 NYLON_EMBED_MODEL=bge-m3 NYLON_EMBED_DIMS=1024 \
./nylon-engine serve 0.0.0.0:50051
```

Optional understanding layer (session-level fact weaving with any OpenAI-compatible chat model):

```bash
NYLON_LLM_URL=https://api.deepseek.com/v1/chat/completions \
NYLON_LLM_MODEL=deepseek-v4-flash NYLON_LLM_API_KEY=<your-key> NYLON_LLM_THINKING_OFF=1 \
./nylon-engine serve 0.0.0.0:50051
```

Verify with the smoke test (runs weave + get_node + resonate + search):

```bash
NYLON_SERVER=http://127.0.0.1:50051 ./smoke_client
# -> SMOKE_OK
```

## Your first memory (CLI)

```bash
export NYLON_SERVER=http://127.0.0.1:50051

# write
./nylon_cli weave --owner alice --task travel --fact "Alice prefers window seats on business trips"
# -> NODE_ID=0

# recall
./nylon_cli resonate --owner alice --query "flight seat preference" --budget 5
# -> ACTIVATED=1
# -> 0  0.812  Alice prefers window seats on business trips
```

`--owner` scopes memories (use a person or project slug). `--tenant` defaults to `codex` and isolates datasets.

## Integrating with your agent / IDE

Any tool that can run a shell command or speak gRPC can use NylonME:

- **Shell-capable agents** (Codex, Cursor, Aider, scripts): call `nylon_cli` with `NYLON_SERVER` pointing at the engine. Recall at task start, weave at milestones.
- **gRPC-native integrations**: the contract is [proto/nylon/v1/memory.proto](../proto/nylon/v1/memory.proto) — `Weave`, `WeaveSession`, `Resonate`, `Search`, `GetNode`.
- **Codex users**: see the `nylonme-memory` plugin pattern — a small skill that teaches the agent when to recall and what to persist. The same pattern ports to any agent framework with custom instructions.

Good facts are self-contained sentences with names, numbers, and paths. Never weave secrets (API keys, passwords).

## Tuning

See the [README](../README.md#tuning-knobs-env-vars) for the full env-var knob table (`NYLON_MAX_SEEDS`, `NYLON_CAT{n}_MAX_HOPS`, `NYLON_RERANK_VEC`, ...).