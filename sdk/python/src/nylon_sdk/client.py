"""Sync and async clients for the NylonME memory engine.

Both clients expose the five RPCs of proto/nylon/v1/memory.proto:
Weave, WeaveSession, Resonate, Search, GetNode.

Endpoint and scoping defaults follow the CLI/MCP conventions:
    NYLON_SERVER  (default "127.0.0.1:50051")
    NYLON_OWNER   (default "default")
    NYLON_TENANT  (default "default")

TLS (L2.5): use an ``https://`` prefixed NYLON_SERVER/target to enable TLS.
For self-signed certificates set NYLON_TLS_CA (or the ``tls_ca`` argument) to
the CA PEM file. If the certificate's SAN does not match the dial target,
pass ``("grpc.ssl_target_name_override", "<name>")`` in channel_options.
"""

from __future__ import annotations

import os
import struct
from collections.abc import Mapping, Sequence
from typing import Any, Optional, Union

import grpc

from ._gen.nylon.v1 import memory_pb2 as pb
from ._gen.nylon.v1 import memory_pb2_grpc as pb_grpc
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

DEFAULT_TARGET = "127.0.0.1:50051"


def _parse_target(target: str) -> tuple[str, bool]:
    """Return (host:port, use_tls). Bare host:port stays plaintext (back-compat)."""
    if target.startswith("https://"):
        return target[len("https://"):], True
    if target.startswith("http://"):
        return target[len("http://"):], False
    return target, False


def _normalize_target(target: str) -> str:
    return _parse_target(target)[0]


def _resolve_tls_ca(tls_ca: Optional[str]) -> Optional[str]:
    return tls_ca or os.environ.get("NYLON_TLS_CA") or None


def _sync_channel(target: str, tls_ca: Optional[str], options: Optional[Sequence]):
    hostport, use_tls = _parse_target(target)
    if use_tls:
        root = None
        ca = _resolve_tls_ca(tls_ca)
        if ca:
            with open(ca, "rb") as f:
                root = f.read()
        creds = grpc.ssl_channel_credentials(root_certificates=root)
        return grpc.secure_channel(hostport, creds, options=options), hostport
    return grpc.insecure_channel(hostport, options=options), hostport


def _aio_channel(target: str, tls_ca: Optional[str], options: Optional[Sequence]):
    hostport, use_tls = _parse_target(target)
    if use_tls:
        root = None
        ca = _resolve_tls_ca(tls_ca)
        if ca:
            with open(ca, "rb") as f:
                root = f.read()
        creds = grpc.ssl_channel_credentials(root_certificates=root)
        return grpc.aio.secure_channel(hostport, creds, options=options), hostport
    return grpc.aio.insecure_channel(hostport, options=options), hostport


def _context(
    task: Optional[str],
    emotion_valence: Optional[float],
    device: Optional[str],
    max_hops: Optional[int],
) -> Optional[pb.ContextSpectrum]:
    if task is None and emotion_valence is None and device is None and max_hops is None:
        return None
    ctx = pb.ContextSpectrum()
    if task is not None:
        ctx.task = task
    if emotion_valence is not None:
        ctx.emotion_valence = emotion_valence
    if device is not None:
        ctx.device = device
    if max_hops is not None:
        ctx.max_hops = max_hops
    return ctx


def _pack_embedding(embedding: Sequence[float]) -> bytes:
    return struct.pack(f"<{len(embedding)}f", *embedding)


def _filaments(f: pb.Filaments) -> Filaments:
    return Filaments(
        fact=f.fact,
        emotion_valence=f.emotion_valence,
        emotion_intensity=f.emotion_intensity,
        created_at=f.created_at,
        decay_rate=f.decay_rate,
        relations=tuple(f.relations),
        confidence=f.confidence,
        mentions_7d=f.mentions_7d,
    )


def _activated(a: pb.ActivatedNode) -> ActivatedNode:
    return ActivatedNode(
        node_id=a.node_id,
        resonance=a.resonance,
        filaments=_filaments(a.filaments),
    )


def _session_events(
    events: Sequence[Union[SessionEventInput, Mapping[str, Any]]],
) -> list:
    out = []
    for e in events:
        if isinstance(e, Mapping):
            out.append(
                pb.SessionEvent(
                    event_id=str(e.get("event_id", "")),
                    speaker=str(e.get("speaker", "")),
                    text=str(e["text"]),
                )
            )
        else:
            out.append(
                pb.SessionEvent(event_id=e.event_id, speaker=e.speaker, text=e.text)
            )
    return out


def _session_result(resp: pb.WeaveSessionResponse) -> SessionResult:
    return SessionResult(
        leaf_nodes=tuple(
            EventNode(event_id=e.event_id, node_id=e.node_id) for e in resp.leaf_nodes
        ),
        fact_nodes=tuple(
            FactNode(
                node_id=f.node_id,
                fact=f.fact,
                source_event_ids=tuple(f.source_event_ids),
            )
            for f in resp.fact_nodes
        ),
        abstract_status=resp.abstract_status,
    )


def _resonate_result(resp: pb.ResonateResponse) -> ResonateResult:
    return ResonateResult(
        activated=tuple(_activated(a) for a in resp.activated),
        seed_ids=tuple(resp.seed_ids),
    )


class NylonClient:
    """Synchronous client. Use as a context manager to close the channel."""

    def __init__(
        self,
        target: Optional[str] = None,
        *,
        owner: Optional[str] = None,
        tenant: Optional[str] = None,
        timeout: float = 30.0,
        channel_options: Optional[Sequence] = None,
        tls_ca: Optional[str] = None,
    ) -> None:
        raw = target or os.environ.get("NYLON_SERVER") or DEFAULT_TARGET
        self._channel, self.target = _sync_channel(raw, tls_ca, channel_options)
        self.owner = owner or os.environ.get("NYLON_OWNER") or "default"
        self.tenant = tenant or os.environ.get("NYLON_TENANT") or "default"
        self.timeout = timeout
        self._stub = pb_grpc.MemoryEngineStub(self._channel)

    def __enter__(self) -> "NylonClient":
        return self

    def __exit__(self, *exc: Any) -> None:
        self.close()

    def close(self) -> None:
        self._channel.close()

    def weave(
        self,
        text: str,
        *,
        task: Optional[str] = None,
        emotion_valence: Optional[float] = None,
        device: Optional[str] = None,
    ) -> WeaveResult:
        resp = self._stub.Weave(
            pb.WeaveRequest(
                tenant_id=self.tenant,
                owner_id=self.owner,
                raw_event=text,
                context=_context(task, emotion_valence, device, None),
            ),
            timeout=self.timeout,
        )
        return WeaveResult(
            node_id=resp.node_id,
            linked_nodes=tuple(resp.linked_nodes),
            conflict_nodes=tuple(resp.conflict_nodes),
        )

    def weave_session(
        self,
        events: Sequence[Union[SessionEventInput, Mapping[str, Any]]],
        *,
        skip_abstract: bool = False,
    ) -> SessionResult:
        resp = self._stub.WeaveSession(
            pb.WeaveSessionRequest(
                tenant_id=self.tenant,
                owner_id=self.owner,
                events=_session_events(events),
                skip_abstract=skip_abstract,
            ),
            timeout=self.timeout,
        )
        return _session_result(resp)

    def resonate(
        self,
        query: str,
        *,
        budget: int = 0,
        top_k: int = 0,
        task: Optional[str] = None,
        emotion_valence: Optional[float] = None,
        device: Optional[str] = None,
        max_hops: Optional[int] = None,
    ) -> ResonateResult:
        resp = self._stub.Resonate(
            pb.ResonateRequest(
                tenant_id=self.tenant,
                owner_id=self.owner,
                query=query,
                context=_context(task, emotion_valence, device, max_hops),
                budget=budget,
                top_k=top_k,
            ),
            timeout=self.timeout,
        )
        return _resonate_result(resp)

    def search(self, embedding: Sequence[float], *, top_k: int = 10) -> list:
        resp = self._stub.Search(
            pb.SearchRequest(
                tenant_id=self.tenant,
                owner_id=self.owner,
                query_embedding=_pack_embedding(embedding),
                top_k=top_k,
            ),
            timeout=self.timeout,
        )
        return [_activated(a) for a in resp.neighbors]

    def get_node(self, node_id: int) -> NodeInfo:
        resp = self._stub.GetNode(
            pb.GetNodeRequest(tenant_id=self.tenant, node_id=node_id),
            timeout=self.timeout,
        )
        return NodeInfo(
            node_id=resp.node_id,
            filaments=_filaments(resp.filaments),
            current_tension=resp.current_tension,
        )

    def feedback(
        self,
        query: str,
        *,
        rating: str = "down",
        comment: str = "",
        shown_node_ids: Optional[list] = None,
    ) -> bool:
        """回答质量回执：报告一次失败的记忆回答（down/wrong/insufficient）。

        引擎持久化记录，并在空闲反思时针对失败簇定向补推断
        （反馈驱动反思，需服务端 NYLON_FEEDBACK_REFLECT=1）。
        """
        resp = self._stub.ReportFeedback(
            pb.FeedbackRequest(
                tenant_id=self.tenant,
                owner_id=self.owner,
                query=query,
                rating=rating,
                comment=comment,
                shown_node_ids=shown_node_ids or [],
            ),
            timeout=self.timeout,
        )
        return resp.recorded


class AsyncNylonClient:
    """Async client built on grpc.aio. Use as an async context manager."""

    def __init__(
        self,
        target: Optional[str] = None,
        *,
        owner: Optional[str] = None,
        tenant: Optional[str] = None,
        timeout: float = 30.0,
        channel_options: Optional[Sequence] = None,
        tls_ca: Optional[str] = None,
    ) -> None:
        raw = target or os.environ.get("NYLON_SERVER") or DEFAULT_TARGET
        self._channel, self.target = _aio_channel(raw, tls_ca, channel_options)
        self.owner = owner or os.environ.get("NYLON_OWNER") or "default"
        self.tenant = tenant or os.environ.get("NYLON_TENANT") or "default"
        self.timeout = timeout
        self._stub = pb_grpc.MemoryEngineStub(self._channel)

    async def __aenter__(self) -> "AsyncNylonClient":
        return self

    async def __aexit__(self, *exc: Any) -> None:
        await self.close()

    async def close(self) -> None:
        await self._channel.close()

    async def weave(
        self,
        text: str,
        *,
        task: Optional[str] = None,
        emotion_valence: Optional[float] = None,
        device: Optional[str] = None,
    ) -> WeaveResult:
        resp = await self._stub.Weave(
            pb.WeaveRequest(
                tenant_id=self.tenant,
                owner_id=self.owner,
                raw_event=text,
                context=_context(task, emotion_valence, device, None),
            ),
            timeout=self.timeout,
        )
        return WeaveResult(
            node_id=resp.node_id,
            linked_nodes=tuple(resp.linked_nodes),
            conflict_nodes=tuple(resp.conflict_nodes),
        )

    async def weave_session(
        self,
        events: Sequence[Union[SessionEventInput, Mapping[str, Any]]],
        *,
        skip_abstract: bool = False,
    ) -> SessionResult:
        resp = await self._stub.WeaveSession(
            pb.WeaveSessionRequest(
                tenant_id=self.tenant,
                owner_id=self.owner,
                events=_session_events(events),
                skip_abstract=skip_abstract,
            ),
            timeout=self.timeout,
        )
        return _session_result(resp)

    async def resonate(
        self,
        query: str,
        *,
        budget: int = 0,
        top_k: int = 0,
        task: Optional[str] = None,
        emotion_valence: Optional[float] = None,
        device: Optional[str] = None,
        max_hops: Optional[int] = None,
    ) -> ResonateResult:
        resp = await self._stub.Resonate(
            pb.ResonateRequest(
                tenant_id=self.tenant,
                owner_id=self.owner,
                query=query,
                context=_context(task, emotion_valence, device, max_hops),
                budget=budget,
                top_k=top_k,
            ),
            timeout=self.timeout,
        )
        return _resonate_result(resp)

    async def search(self, embedding: Sequence[float], *, top_k: int = 10) -> list:
        resp = await self._stub.Search(
            pb.SearchRequest(
                tenant_id=self.tenant,
                owner_id=self.owner,
                query_embedding=_pack_embedding(embedding),
                top_k=top_k,
            ),
            timeout=self.timeout,
        )
        return [_activated(a) for a in resp.neighbors]

    async def get_node(self, node_id: int) -> NodeInfo:
        resp = await self._stub.GetNode(
            pb.GetNodeRequest(tenant_id=self.tenant, node_id=node_id),
            timeout=self.timeout,
        )
        return NodeInfo(
            node_id=resp.node_id,
            filaments=_filaments(resp.filaments),
            current_tension=resp.current_tension,
        )

    async def feedback(
        self,
        query: str,
        *,
        rating: str = "down",
        comment: str = "",
        shown_node_ids: Optional[list] = None,
    ) -> bool:
        """回答质量回执（反馈驱动反思的输入信号），语义同同步版。"""
        resp = await self._stub.ReportFeedback(
            pb.FeedbackRequest(
                tenant_id=self.tenant,
                owner_id=self.owner,
                query=query,
                rating=rating,
                comment=comment,
                shown_node_ids=shown_node_ids or [],
            ),
            timeout=self.timeout,
        )
        return resp.recorded
