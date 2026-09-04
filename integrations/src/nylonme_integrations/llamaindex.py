"""LlamaIndex adapter for NylonME.

Exposes :class:`NylonMeRetriever`, which wraps NylonME resonance recall as a
LlamaIndex retriever returning ``NodeWithScore`` objects.

``llama-index-core`` is a required dependency of this module; install it with
``pip install nylonme-integrations[llamaindex]``.
"""

from __future__ import annotations

from typing import Any, Optional

try:
    from llama_index.core.retrievers import BaseRetriever
    from llama_index.core.schema import NodeWithScore, QueryBundle, TextNode
except ImportError as exc:  # pragma: no cover
    raise ImportError(
        "This adapter requires llama-index-core. "
        "Install it with: pip install nylonme-integrations[llamaindex]"
    ) from exc

from ._core import build_client, node_metadata, resonate_records


class NylonMeRetriever(BaseRetriever):
    """LlamaIndex retriever backed by NylonME resonance recall."""

    def __init__(
        self,
        target: str = "127.0.0.1:50051",
        owner: str = "default",
        tenant: str = "default",
        budget: int = 5,
        task: Optional[str] = None,
        **kwargs: Any,
    ) -> None:
        # llama-index-core >= 0.11 BaseRetriever is a plain class with an
        # explicit __init__ (no pydantic fields), so config must be stored
        # as instance attributes after super().__init__.
        super().__init__(**kwargs)
        self.target = target
        self.owner = owner
        self.tenant = tenant
        self.budget = budget
        self.task = task

    def _retrieve(self, query_bundle: QueryBundle) -> list[NodeWithScore]:
        query = query_bundle.query_str
        with build_client(self.target, self.owner, self.tenant) as client:
            records = resonate_records(client, query, budget=self.budget, task=self.task)
        return [
            NodeWithScore(
                node=TextNode(
                    text=r["filaments"]["fact"],
                    id_=str(r["node_id"]),
                    metadata=node_metadata(r),
                ),
                score=float(r["resonance"]),
            )
            for r in records
        ]

    async def _aretrieve(self, query_bundle: QueryBundle) -> list[NodeWithScore]:
        # Sync SDK call on the event loop for a simple, correct default.
        return self._retrieve(query_bundle)


__all__ = ["NylonMeRetriever"]
