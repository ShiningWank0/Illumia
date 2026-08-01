"""Internal value objects shared by ML backends."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Literal, Protocol


@dataclass(frozen=True, slots=True)
class QualityThresholds:
    tau_high: float = 0.82
    tau_low: float = 0.55
    min_cluster_size: int = 3
    min_face_size: int = 48
    min_det_conf: float = 0.5
    min_visibility: float = 0.6


@dataclass(frozen=True, slots=True)
class ModelBundleInfo:
    name: str
    version: str
    sha256: str


@dataclass(frozen=True, slots=True)
class Instance:
    kind: Literal["person", "head", "face"]
    bbox: tuple[float, float, float, float]
    det_conf: float
    quality_passed: bool
    quality_flags: tuple[str, ...]
    embedding: bytes
    embedding_dim: int


@dataclass(frozen=True, slots=True)
class Analysis:
    model_version: str
    instances: tuple[Instance, ...]


class Backend(Protocol):
    """Stateless image inference backend."""

    name: Literal["onnx", "mock"]
    model_bundle: ModelBundleInfo | None
    providers: tuple[str, ...]
    thresholds: QualityThresholds

    def analyze(self, image_bytes: bytes, *, tagger: bool = False) -> Analysis: ...


class BackendUnavailable(RuntimeError):
    """Raised when a selected inference backend cannot be used."""
