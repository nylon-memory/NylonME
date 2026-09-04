"""Framework-agnostic bridge between NylonME and RAG/agent frameworks.

This module only depends on ``nylon-sdk``. The LangChain and LlamaIndex
adapters import it, so the same resonate/weave semantics are shared and can
be tested without pulling in either framework.
"""

from __future__ import annotations

from typing import Any, Optional

from nylon_sdk import NylonClient


def build_client(
    target: str,
    owner: str,
    tenant: str,
    *,
    timeout: float = 30.0,
) -> NylonClient:
    """Create a NylonME client from the values the adapters expose as fields."""
    return NylonClient(target, owner=owner, tenant=tenant, timeout=timeout)


def node_metadata(record: dict[str, Any]) -> dict[str, Any]:
    """Flatten one ActivatedNode into stable, JSON-friendly metadata."""
    f = record["filaments"]
    return {
        "node_id": record["node_id"],
        "resonance": record["resonance"],
        "relations": f["relations"],
        "confidence": f["confidence"],
        "emotion_valence": f["emotion_valence"],
        "emotion_intensity": f["emotion_intensity"],
        "mentions_7d": f["mentions_7d"],
    }


def resonate_records(
    client: NylonClient,
    query: str,
    *,
    budget: int = 5,
    task: Optional[str] = None,
) -> list[dict[str, Any]]:
    """Return resonated memories as plain dictionaries."""
    result = client.resonate(query, budget=budget, task=task)
    return [
        {
            "node_id": a.node_id,
            "resonance": a.resonance,
            "filaments": {
                "fact": a.filaments.fact,
                "relations": list(a.filaments.relations),
                "confidence": a.filaments.confidence,
                "emotion_valence": a.filaments.emotion_valence,
                "emotion_intensity": a.filaments.emotion_intensity,
                "mentions_7d": a.filaments.mentions_7d,
            },
        }
        for a in result.activated
    ]
