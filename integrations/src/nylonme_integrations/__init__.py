"""NylonME framework adapters.

A thin bridge from NylonME to the two most common Python agent/RAG
frameworks. The core resonance logic lives in ``_core.py`` and only depends
on ``nylon-sdk``; the framework modules import their framework lazily.

    # LangChain
    from nylonme_integrations.langchain import NylonMeRetriever, NylonMeMemory

    # LlamaIndex
    from nylonme_integrations.llamaindex import NylonMeRetriever
"""

from ._core import build_client, node_metadata, resonate_records

__version__ = "0.1.0"

__all__ = ["build_client", "node_metadata", "resonate_records", "__version__"]
