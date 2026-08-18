"""Quickstart: weave a memory and resonate it back.

Run an engine first (see docs/GETTING_STARTED.md), then:

    python examples/quickstart.py
"""

import asyncio
import os

from nylon_sdk import AsyncNylonClient


async def main() -> None:
    target = os.environ.get("NYLON_SERVER", "127.0.0.1:50051")
    async with AsyncNylonClient(target, owner="quickstart") as client:
        result = await client.weave(
            "Alice prefers window seats on business trips", task="travel"
        )
        print(f"wove node {result.node_id}")

        recalled = await client.resonate("flight seat preference", budget=5)
        print(f"seeds: {list(recalled.seed_ids)}")
        for node in recalled.activated:
            print(f"{node.node_id}\t{node.resonance:.3f}\t{node.filaments.fact}")


if __name__ == "__main__":
    asyncio.run(main())