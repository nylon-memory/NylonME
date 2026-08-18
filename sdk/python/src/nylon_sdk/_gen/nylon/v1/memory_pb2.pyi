from google.protobuf.internal import containers as _containers
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class Filaments(_message.Message):
    __slots__ = ("fact", "emotion_valence", "emotion_intensity", "created_at", "decay_rate", "relations", "confidence", "mentions_7d")
    FACT_FIELD_NUMBER: _ClassVar[int]
    EMOTION_VALENCE_FIELD_NUMBER: _ClassVar[int]
    EMOTION_INTENSITY_FIELD_NUMBER: _ClassVar[int]
    CREATED_AT_FIELD_NUMBER: _ClassVar[int]
    DECAY_RATE_FIELD_NUMBER: _ClassVar[int]
    RELATIONS_FIELD_NUMBER: _ClassVar[int]
    CONFIDENCE_FIELD_NUMBER: _ClassVar[int]
    MENTIONS_7D_FIELD_NUMBER: _ClassVar[int]
    fact: str
    emotion_valence: float
    emotion_intensity: float
    created_at: int
    decay_rate: float
    relations: _containers.RepeatedScalarFieldContainer[str]
    confidence: float
    mentions_7d: int
    def __init__(self, fact: _Optional[str] = ..., emotion_valence: _Optional[float] = ..., emotion_intensity: _Optional[float] = ..., created_at: _Optional[int] = ..., decay_rate: _Optional[float] = ..., relations: _Optional[_Iterable[str]] = ..., confidence: _Optional[float] = ..., mentions_7d: _Optional[int] = ...) -> None: ...

class ContextSpectrum(_message.Message):
    __slots__ = ("task", "emotion_valence", "device", "max_hops")
    TASK_FIELD_NUMBER: _ClassVar[int]
    EMOTION_VALENCE_FIELD_NUMBER: _ClassVar[int]
    DEVICE_FIELD_NUMBER: _ClassVar[int]
    MAX_HOPS_FIELD_NUMBER: _ClassVar[int]
    task: str
    emotion_valence: float
    device: str
    max_hops: int
    def __init__(self, task: _Optional[str] = ..., emotion_valence: _Optional[float] = ..., device: _Optional[str] = ..., max_hops: _Optional[int] = ...) -> None: ...

class WeaveRequest(_message.Message):
    __slots__ = ("tenant_id", "owner_id", "raw_event", "context")
    TENANT_ID_FIELD_NUMBER: _ClassVar[int]
    OWNER_ID_FIELD_NUMBER: _ClassVar[int]
    RAW_EVENT_FIELD_NUMBER: _ClassVar[int]
    CONTEXT_FIELD_NUMBER: _ClassVar[int]
    tenant_id: str
    owner_id: str
    raw_event: str
    context: ContextSpectrum
    def __init__(self, tenant_id: _Optional[str] = ..., owner_id: _Optional[str] = ..., raw_event: _Optional[str] = ..., context: _Optional[_Union[ContextSpectrum, _Mapping]] = ...) -> None: ...

class WeaveResponse(_message.Message):
    __slots__ = ("node_id", "linked_nodes", "conflict_nodes")
    NODE_ID_FIELD_NUMBER: _ClassVar[int]
    LINKED_NODES_FIELD_NUMBER: _ClassVar[int]
    CONFLICT_NODES_FIELD_NUMBER: _ClassVar[int]
    node_id: int
    linked_nodes: _containers.RepeatedScalarFieldContainer[int]
    conflict_nodes: _containers.RepeatedScalarFieldContainer[int]
    def __init__(self, node_id: _Optional[int] = ..., linked_nodes: _Optional[_Iterable[int]] = ..., conflict_nodes: _Optional[_Iterable[int]] = ...) -> None: ...

class ResonateRequest(_message.Message):
    __slots__ = ("tenant_id", "owner_id", "query", "context", "budget")
    TENANT_ID_FIELD_NUMBER: _ClassVar[int]
    OWNER_ID_FIELD_NUMBER: _ClassVar[int]
    QUERY_FIELD_NUMBER: _ClassVar[int]
    CONTEXT_FIELD_NUMBER: _ClassVar[int]
    BUDGET_FIELD_NUMBER: _ClassVar[int]
    tenant_id: str
    owner_id: str
    query: str
    context: ContextSpectrum
    budget: int
    def __init__(self, tenant_id: _Optional[str] = ..., owner_id: _Optional[str] = ..., query: _Optional[str] = ..., context: _Optional[_Union[ContextSpectrum, _Mapping]] = ..., budget: _Optional[int] = ...) -> None: ...

class ActivatedNode(_message.Message):
    __slots__ = ("node_id", "resonance", "filaments")
    NODE_ID_FIELD_NUMBER: _ClassVar[int]
    RESONANCE_FIELD_NUMBER: _ClassVar[int]
    FILAMENTS_FIELD_NUMBER: _ClassVar[int]
    node_id: int
    resonance: float
    filaments: Filaments
    def __init__(self, node_id: _Optional[int] = ..., resonance: _Optional[float] = ..., filaments: _Optional[_Union[Filaments, _Mapping]] = ...) -> None: ...

class ResonateResponse(_message.Message):
    __slots__ = ("activated", "seed_ids")
    ACTIVATED_FIELD_NUMBER: _ClassVar[int]
    SEED_IDS_FIELD_NUMBER: _ClassVar[int]
    activated: _containers.RepeatedCompositeFieldContainer[ActivatedNode]
    seed_ids: _containers.RepeatedScalarFieldContainer[int]
    def __init__(self, activated: _Optional[_Iterable[_Union[ActivatedNode, _Mapping]]] = ..., seed_ids: _Optional[_Iterable[int]] = ...) -> None: ...

class SearchRequest(_message.Message):
    __slots__ = ("tenant_id", "owner_id", "query_embedding", "top_k")
    TENANT_ID_FIELD_NUMBER: _ClassVar[int]
    OWNER_ID_FIELD_NUMBER: _ClassVar[int]
    QUERY_EMBEDDING_FIELD_NUMBER: _ClassVar[int]
    TOP_K_FIELD_NUMBER: _ClassVar[int]
    tenant_id: str
    owner_id: str
    query_embedding: bytes
    top_k: int
    def __init__(self, tenant_id: _Optional[str] = ..., owner_id: _Optional[str] = ..., query_embedding: _Optional[bytes] = ..., top_k: _Optional[int] = ...) -> None: ...

class SearchResponse(_message.Message):
    __slots__ = ("neighbors",)
    NEIGHBORS_FIELD_NUMBER: _ClassVar[int]
    neighbors: _containers.RepeatedCompositeFieldContainer[ActivatedNode]
    def __init__(self, neighbors: _Optional[_Iterable[_Union[ActivatedNode, _Mapping]]] = ...) -> None: ...

class GetNodeRequest(_message.Message):
    __slots__ = ("tenant_id", "node_id")
    TENANT_ID_FIELD_NUMBER: _ClassVar[int]
    NODE_ID_FIELD_NUMBER: _ClassVar[int]
    tenant_id: str
    node_id: int
    def __init__(self, tenant_id: _Optional[str] = ..., node_id: _Optional[int] = ...) -> None: ...

class GetNodeResponse(_message.Message):
    __slots__ = ("node_id", "filaments", "current_tension")
    NODE_ID_FIELD_NUMBER: _ClassVar[int]
    FILAMENTS_FIELD_NUMBER: _ClassVar[int]
    CURRENT_TENSION_FIELD_NUMBER: _ClassVar[int]
    node_id: int
    filaments: Filaments
    current_tension: float
    def __init__(self, node_id: _Optional[int] = ..., filaments: _Optional[_Union[Filaments, _Mapping]] = ..., current_tension: _Optional[float] = ...) -> None: ...

class SessionEvent(_message.Message):
    __slots__ = ("event_id", "speaker", "text")
    EVENT_ID_FIELD_NUMBER: _ClassVar[int]
    SPEAKER_FIELD_NUMBER: _ClassVar[int]
    TEXT_FIELD_NUMBER: _ClassVar[int]
    event_id: str
    speaker: str
    text: str
    def __init__(self, event_id: _Optional[str] = ..., speaker: _Optional[str] = ..., text: _Optional[str] = ...) -> None: ...

class WeaveSessionRequest(_message.Message):
    __slots__ = ("tenant_id", "owner_id", "events", "skip_abstract")
    TENANT_ID_FIELD_NUMBER: _ClassVar[int]
    OWNER_ID_FIELD_NUMBER: _ClassVar[int]
    EVENTS_FIELD_NUMBER: _ClassVar[int]
    SKIP_ABSTRACT_FIELD_NUMBER: _ClassVar[int]
    tenant_id: str
    owner_id: str
    events: _containers.RepeatedCompositeFieldContainer[SessionEvent]
    skip_abstract: bool
    def __init__(self, tenant_id: _Optional[str] = ..., owner_id: _Optional[str] = ..., events: _Optional[_Iterable[_Union[SessionEvent, _Mapping]]] = ..., skip_abstract: _Optional[bool] = ...) -> None: ...

class EventNode(_message.Message):
    __slots__ = ("event_id", "node_id")
    EVENT_ID_FIELD_NUMBER: _ClassVar[int]
    NODE_ID_FIELD_NUMBER: _ClassVar[int]
    event_id: str
    node_id: int
    def __init__(self, event_id: _Optional[str] = ..., node_id: _Optional[int] = ...) -> None: ...

class FactNode(_message.Message):
    __slots__ = ("node_id", "fact", "source_event_ids")
    NODE_ID_FIELD_NUMBER: _ClassVar[int]
    FACT_FIELD_NUMBER: _ClassVar[int]
    SOURCE_EVENT_IDS_FIELD_NUMBER: _ClassVar[int]
    node_id: int
    fact: str
    source_event_ids: _containers.RepeatedScalarFieldContainer[str]
    def __init__(self, node_id: _Optional[int] = ..., fact: _Optional[str] = ..., source_event_ids: _Optional[_Iterable[str]] = ...) -> None: ...

class WeaveSessionResponse(_message.Message):
    __slots__ = ("leaf_nodes", "fact_nodes")
    LEAF_NODES_FIELD_NUMBER: _ClassVar[int]
    FACT_NODES_FIELD_NUMBER: _ClassVar[int]
    leaf_nodes: _containers.RepeatedCompositeFieldContainer[EventNode]
    fact_nodes: _containers.RepeatedCompositeFieldContainer[FactNode]
    def __init__(self, leaf_nodes: _Optional[_Iterable[_Union[EventNode, _Mapping]]] = ..., fact_nodes: _Optional[_Iterable[_Union[FactNode, _Mapping]]] = ...) -> None: ...
