"""LangChain adapter for NylonME.

Exposes:

* :class:`NylonMeRetriever` - RAG retrieval over resonated memories.
* :class:`NylonMeMemory` - conversational memory that weaves turns and
  injects the most relevant memories into the prompt.

``langchain-core`` is a required dependency of this module; install it with
``pip install nylonme-integrations[langchain]``.
"""

from __future__ import annotations

from typing import Any, Optional

try:
    from langchain_core.callbacks.manager import CallbackManagerForRetrieverRun
    from langchain_core.documents import Document
    from langchain_core.memory import BaseMemory
    from langchain_core.retrievers import BaseRetriever
except ImportError as exc:  # pragma: no cover
    raise ImportError(
        "This adapter requires langchain-core. "
        "Install it with: pip install nylonme-integrations[langchain]"
    ) from exc

from ._core import build_client, node_metadata, resonate_records


class NylonMeRetriever(BaseRetriever):
    """Retriever that maps a query to resonated NylonME memories as Documents."""

    target: str = "127.0.0.1:50051"
    owner: str = "default"
    tenant: str = "default"
    budget: int = 5
    task: Optional[str] = None

    def _get_relevant_documents(
        self,
        query: str,
        *,
        run_manager: CallbackManagerForRetrieverRun,
    ) -> list[Document]:
        del run_manager
        with build_client(self.target, self.owner, self.tenant) as client:
            records = resonate_records(client, query, budget=self.budget, task=self.task)
        return [
            Document(page_content=r["filaments"]["fact"], metadata=node_metadata(r))
            for r in records
        ]

    async def _aget_relevant_documents(
        self,
        query: str,
        *,
        run_manager=None,
    ) -> list[Document]:
        del run_manager
        # The sync SDK call is the simplest correct path; async callers can
        # use AsyncNylonClient directly if they need a truly async channel.
        return self._get_relevant_documents(query, run_manager=run_manager)


class NylonMeMemory(BaseMemory):
    """Conversational memory: weaves each turn, injects recall into prompts.

    Wire it into a prompt that contains a ``{memory}`` placeholder (or the
    ``memory_key`` you configure). On ``save_context`` the latest user input
    and model output are woven into the engine; on ``load_memory_variables``
    the engine resonates on the current input and returns the matched facts.
    """

    target: str = "127.0.0.1:50051"
    owner: str = "default"
    tenant: str = "default"
    budget: int = 4
    memory_key: str = "memory"
    input_key: str = "input"

    @property
    def memory_variables(self) -> list[str]:
        return [self.memory_key]

    def load_memory_variables(self, inputs: dict[str, Any]) -> dict[str, str]:
        query = self._query_from(inputs)
        if not query:
            return {self.memory_key: ""}
        with build_client(self.target, self.owner, self.tenant) as client:
            records = resonate_records(client, query, budget=self.budget)
        facts = [r["filaments"]["fact"] for r in records]
        return {self.memory_key: "\n".join(f"- {f}" for f in facts)}

    def save_context(self, inputs: dict[str, Any], outputs: dict[str, str]) -> None:
        text = " ".join(
            str(v) for v in list(inputs.values()) + list(outputs.values()) if v
        ).strip()
        if text:
            with build_client(self.target, self.owner, self.tenant) as client:
                client.weave(text)

    def clear(self) -> None:
        # The community engine has no destructive clear; memories decay via
        # the tension model rather than being force-deleted.
        return None

    def _query_from(self, inputs: dict[str, Any]) -> str:
        if self.input_key in inputs:
            return str(inputs[self.input_key])
        return " ".join(str(v) for v in inputs.values() if v).strip()


__all__ = ["NylonMeRetriever", "NylonMeMemory"]
