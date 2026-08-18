# nylon-sdk

Official Python client for the [NylonME](https://github.com/nylon-memory/NylonME) memory engine.

## Install

```bash
pip install nylon-sdk            # from PyPI
pip install ./sdk/python         # from this repo
```

## Quick start

Start an engine (see [docs/GETTING_STARTED.md](../../docs/GETTING_STARTED.md)), then:

```python
from nylon_sdk import NylonClient

with NylonClient("127.0.0.1:50051", owner="alice") as client:
    # weave a memory
    client.weave("Alice prefers window seats on business trips", task="travel")

    # recall by situational resonance
    result = client.resonate("flight seat preference", budget=5)
    for node in result.activated:
        print(node.node_id, node.resonance, node.filaments.fact)
```

Async:

```python
import asyncio
from nylon_sdk import AsyncNylonClient

async def main():
    async with AsyncNylonClient("127.0.0.1:50051", owner="alice") as client:
        await client.weave("Bob moved to Berlin in 2024")
        result = await client.resonate("where does Bob live")
        print([n.filaments.fact for n in result.activated])

asyncio.run(main())
```

Batch session ingestion (two-tier write: leaf events + LLM-abstracted facts):

```python
client.weave_session([
    {"speaker": "user", "text": "I booked the 8am flight", "event_id": "e1"},
    {"speaker": "agent", "text": "Noted, window seat", "event_id": "e2"},
])
```

Vector search with your own embeddings (packed little-endian f32 under the hood):

```python
neighbors = client.search(embedding, top_k=10)
```

## Configuration

| Setting | Default | Env fallback |
|---|---|---|
| `target` | `127.0.0.1:50051` | `NYLON_SERVER` |
| `owner` | `default` | `NYLON_OWNER` |
| `tenant` | `default` | `NYLON_TENANT` |
| `timeout` | 30s per call | - |

`owner` scopes memories per person/project; `tenant` isolates datasets entirely.

## API surface

| Method | RPC |
|---|---|
| `weave(text, task=...)` | `Weave` |
| `weave_session(events, skip_abstract=...)` | `WeaveSession` |
| `resonate(query, budget=..., max_hops=...)` | `Resonate` |
| `search(embedding, top_k=...)` | `Search` |
| `get_node(node_id)` | `GetNode` |

`max_hops=0` turns resonance into precise recall (seed-only, no graph
spreading); omit it for the engine default.

## Regenerating stubs

The generated code under `src/nylon_sdk/_gen/` is checked in. To regenerate
after a proto change:

```bash
pip install grpcio-tools
python sdk/python/tools/codegen.py
```