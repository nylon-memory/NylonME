"""Friendly return types for the NylonME Python SDK.

These mirror proto/nylon/v1/memory.proto but use plain dataclasses so
callers never have to touch generated protobuf code.
"""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True)
class Filaments:
    """The six memory filaments of a node (see the memory model doc)."""

    fact: str = ""
    emotion_valence: float = 0.0
    emotion_intensity: float = 0.0
    created_at: int = 0
    decay_rate: float = 0.0
    relations: tuple[str, ...] = ()
    confidence: float = 0.0
    mentions_7d: int = 0


@dataclass(frozen=True)
class ActivatedNode:
    """A node returned by Resonate/Search with its resonance score."""

    node_id: int
    resonance: float
    filaments: Filaments


@dataclass(frozen=True)
class WeaveResult:
    node_id: int
    linked_nodes: tuple[int, ...] = ()
    conflict_nodes: tuple[int, ...] = ()


@dataclass(frozen=True)
class ResonateResult:
    activated: tuple[ActivatedNode, ...]
    seed_ids: tuple[int, ...] = ()


@dataclass(frozen=True)
class SessionEventInput:
    """One event for weave_session (speaker turn, note, tool output, ...)."""

    text: str
    speaker: str = ""
    event_id: str = ""


@dataclass(frozen=True)
class EventNode:
    event_id: str
    node_id: int


@dataclass(frozen=True)
class FactNode:
    node_id: int
    fact: str
    source_event_ids: tuple[str, ...] = ()


@dataclass(frozen=True)
class SessionResult:
    leaf_nodes: tuple[EventNode, ...] = ()
    fact_nodes: tuple[FactNode, ...] = ()


@dataclass(frozen=True)
class NodeInfo:
    node_id: int
    filaments: Filaments
    current_tension: float