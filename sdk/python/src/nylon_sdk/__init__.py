"""NylonME Python SDK: client for the NylonME memory engine.

Quick start:

    from nylon_sdk import NylonClient

    with NylonClient("127.0.0.1:50051", owner="alice") as client:
        client.weave("Alice prefers window seats on business trips")
        for node in client.resonate("flight seat preference", budget=5).activated:
            print(node.filaments.fact)
"""

from .client import AsyncNylonClient, NylonClient
from .types import (
    ActivatedNode,
    EventNode,
    FactNode,
    Filaments,
    NodeInfo,
    ResonateResult,
    SessionEventInput,
    SessionResult,
    WeaveResult,
)

__version__ = "0.2.1"

__all__ = [
    "AsyncNylonClient",
    "NylonClient",
    "ActivatedNode",
    "EventNode",
    "FactNode",
    "Filaments",
    "NodeInfo",
    "ResonateResult",
    "SessionEventInput",
    "SessionResult",
    "WeaveResult",
    "__version__",
]