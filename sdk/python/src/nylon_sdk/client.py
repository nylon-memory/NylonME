"""Sync and async clients for the NylonME memory engine.

Both clients expose the five RPCs of proto/nylon/v1/memory.proto:
Weave, WeaveSession, Resonate, Search, GetNode.

Endpoint and scoping defaults follow the CLI/MCP conventions:
    NYLON_SERVER  (default "127.0.0.1:50051")
    NYLON_OWNER   (default "default")
    NYLON_TENANT  (default "default")
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


def _normalize_target(target: str) -> str:
    # Accept "host:port" or "http(s)://host:port" (matches NYLON_SERVER docs).
    for scheme in ("https://", "http://"):
        if target.startswith(scheme):
            return target[len(scheme):]
    return target


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
    ) -> None:
        self.target = _normalize_target(
            target or os.environ.get("NYLON_SERVER") or DEFAULT_TARGET
        )
        self.owner = owner or os.environ.get("NYLON_OWNER") or "default"
        self.tenant = tenant or os.environ.get("NYLON_TENANT") or "default"
        self.timeout = timeout
        self._channel = grpc.insecure_channel(self.target, options=channel_options)
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
    ) -> None:
        self.target = _normalize_target(
            target or os.environ.get("NYLON_SERVER") or DEFAULT_TARGET
        )
        self.owner = owner or os.environ.get("NYLON_OWNER") or "default"
        self.tenant = tenant or os.environ.get("NYLON_TENANT") or "default"
        self.timeout = timeout
        self._channel = grpc.aio.insecure_channel(self.target, options=channel_options)
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