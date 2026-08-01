"""Shared deterministic two-threshold open-set clustering."""

from __future__ import annotations

import math
import struct
from collections.abc import Iterable, Mapping, Sequence
from dataclasses import dataclass

from .models import QualityThresholds


@dataclass(frozen=True, slots=True)
class ClusterParams:
    tau_high: float
    tau_low: float
    min_cluster_size: int


def params_from_request(values: Mapping[str, object], defaults: QualityThresholds) -> ClusterParams:
    params = ClusterParams(
        tau_high=float(values.get("tau_high", defaults.tau_high)),
        tau_low=float(values.get("tau_low", defaults.tau_low)),
        min_cluster_size=int(values.get("min_cluster_size", defaults.min_cluster_size)),
    )
    if not 0.0 <= params.tau_low <= params.tau_high <= 1.0:
        raise ValueError("thresholds must satisfy 0 <= tau_low <= tau_high <= 1")
    if params.min_cluster_size < 1:
        raise ValueError("min_cluster_size must be positive")
    return params


def decode_f32(value: object, shape: Sequence[object]) -> list[list[float]]:
    if len(shape) != 2:
        raise ValueError("shape must contain two dimensions")
    rows, dim = int(shape[0]), int(shape[1])
    if rows < 0 or dim <= 0:
        raise ValueError("invalid embedding shape")
    if isinstance(value, str):
        import base64

        try:
            value = base64.b64decode(value, validate=True)
        except ValueError as exc:
            raise ValueError("invalid base64 embeddings") from exc
    if isinstance(value, list):
        if rows == 1 and (not value or not isinstance(value[0], list)):
            value = [value]
        if len(value) != rows:
            raise ValueError("embedding row count does not match shape")
        matrix = [[float(item) for item in row] for row in value]
        if any(len(row) != dim for row in matrix):
            raise ValueError("embedding dimension does not match shape")
        return matrix
    if not isinstance(value, bytes | bytearray | memoryview):
        raise ValueError("embeddings must be f32 bytes, base64, or an array")
    raw = bytes(value)
    if len(raw) != rows * dim * 4:
        raise ValueError("embedding byte length does not match shape")
    flat = struct.unpack(f"<{rows * dim}f", raw)
    return [list(flat[index * dim : (index + 1) * dim]) for index in range(rows)]


def _normalize(vector: Sequence[float]) -> list[float]:
    norm = math.sqrt(sum(value * value for value in vector))
    if not math.isfinite(norm) or norm == 0.0:
        raise ValueError("embeddings must be finite and non-zero")
    result = [value / norm for value in vector]
    if not all(math.isfinite(value) for value in result):
        raise ValueError("embeddings must be finite")
    return result


def _similarity(left: Sequence[float], right: Sequence[float]) -> float:
    return max(-1.0, min(1.0, sum(a * b for a, b in zip(left, right, strict=True))))


def _medoid(members: Sequence[int], vectors: Sequence[Sequence[float]]) -> int:
    return max(
        members,
        key=lambda candidate: (
            sum(_similarity(vectors[candidate], vectors[other]) for other in members),
            -candidate,
        ),
    )


def _confirmed_ids(value: object) -> set[str]:
    if isinstance(value, dict):
        return {str(item) for item in value}
    if not isinstance(value, list):
        return set()
    result: set[str] = set()
    for item in value:
        if isinstance(item, list | tuple) and item:
            result.add(str(item[0]))
        elif isinstance(item, dict) and "id" in item:
            result.add(str(item["id"]))
        elif isinstance(item, str):
            result.add(item)
    return result


def _rejection_pairs(value: object) -> set[tuple[str, str]]:
    if not isinstance(value, list):
        return set()
    return {
        (str(item[0]), str(item[1]))
        for item in value
        if isinstance(item, list | tuple) and len(item) == 2
    }


def _assignment(
    identifier: str, cluster: str | None, similarity: float, params: ClusterParams
) -> dict:
    if cluster is None or similarity < params.tau_low:
        state = "unassigned"
        cluster = None
    elif similarity >= params.tau_high:
        state = "auto"
    else:
        state = "candidate"
    return {
        "id": identifier,
        "cluster": cluster,
        "state": state,
        "similarity": round(float(similarity), 7),
    }


def cluster_full(
    ids: Sequence[str],
    vectors: Sequence[Sequence[float]],
    params: ClusterParams,
    *,
    rejections: object = None,
    confirmed: object = None,
) -> dict:
    normalized = [_normalize(vector) for vector in vectors]
    confirmed_ids = _confirmed_ids(confirmed)
    rejected = _rejection_pairs(rejections)
    available = [index for index, identifier in enumerate(ids) if identifier not in confirmed_ids]
    clusters: list[tuple[str, list[int], int]] = []

    while available:
        tmp_id = f"c{len(clusters)}"
        best_pair: tuple[float, int, int] | None = None
        for position, left in enumerate(available):
            if (ids[left], tmp_id) in rejected:
                continue
            for right in available[position + 1 :]:
                if (ids[right], tmp_id) in rejected:
                    continue
                candidate = (_similarity(normalized[left], normalized[right]), -left, -right)
                if best_pair is None or candidate > best_pair:
                    best_pair = candidate
        if best_pair is None or best_pair[0] < params.tau_low:
            break
        members = [-best_pair[1], -best_pair[2]]
        available = [index for index in available if index not in members]
        while available:
            medoid = _medoid(members, normalized)
            choices = [
                (_similarity(normalized[index], normalized[medoid]), -index, index)
                for index in available
                if (ids[index], tmp_id) not in rejected
            ]
            if not choices:
                break
            similarity, _, selected = max(choices)
            if similarity < params.tau_low:
                break
            members.append(selected)
            available.remove(selected)
        clusters.append((tmp_id, members, _medoid(members, normalized)))

    retained = [cluster for cluster in clusters if len(cluster[1]) >= params.min_cluster_size]
    assignments_by_index: dict[int, dict] = {}
    new_clusters: list[dict] = []
    for tmp_id, members, medoid in retained:
        member_ids = [ids[index] for index in members]
        new_clusters.append(
            {"tmp_id": tmp_id, "member_ids": member_ids, "medoid_ids": [ids[medoid]]}
        )
        for index in members:
            similarity = _similarity(normalized[index], normalized[medoid])
            assignments_by_index[index] = _assignment(ids[index], tmp_id, similarity, params)

    medoids = [medoid for _, _, medoid in retained]
    for index in available + [item for _, members, _ in clusters for item in members]:
        if index in assignments_by_index or ids[index] in confirmed_ids:
            continue
        similarity = max(
            (_similarity(normalized[index], normalized[medoid]) for medoid in medoids),
            default=0.0,
        )
        assignments_by_index[index] = _assignment(ids[index], None, similarity, params)
    assignments = [assignments_by_index[index] for index in sorted(assignments_by_index)]
    return {"assignments": assignments, "new_clusters": new_clusters}


def cluster_assign(
    ids: Sequence[str],
    vectors: Sequence[Sequence[float]],
    medoids: Mapping[str, Sequence[float]],
    params: ClusterParams,
    *,
    rejections: object = None,
    confirmed: object = None,
) -> dict:
    normalized = [_normalize(vector) for vector in vectors]
    normalized_medoids = {cluster: _normalize(vector) for cluster, vector in medoids.items()}
    confirmed_ids = _confirmed_ids(confirmed)
    rejected = _rejection_pairs(rejections)
    assignments = []
    for identifier, vector in zip(ids, normalized, strict=True):
        if identifier in confirmed_ids:
            continue
        choices = [
            (_similarity(vector, medoid), cluster)
            for cluster, medoid in normalized_medoids.items()
            if (identifier, cluster) not in rejected
        ]
        similarity, cluster = max(choices, default=(0.0, None), key=lambda item: (item[0], item[1]))
        assignments.append(_assignment(identifier, cluster, similarity, params))
    return {"assignments": assignments, "new_clusters": []}


def validate_ids(ids: object, vectors: Sequence[Sequence[float]]) -> list[str]:
    if not isinstance(ids, list) or len(ids) != len(vectors):
        raise ValueError("ids must match the embedding row count")
    result = [str(identifier) for identifier in ids]
    if len(set(result)) != len(result):
        raise ValueError("ids must be unique")
    return result


def decode_medoids(value: object, dim: int) -> dict[str, list[float]]:
    if not isinstance(value, dict):
        raise ValueError("assign mode requires medoids")
    result: dict[str, list[float]] = {}
    for cluster, encoded in value.items():
        result[str(cluster)] = decode_f32(encoded, [1, dim])[0]
    return result


def iter_f32_bytes(vectors: Iterable[Sequence[float]]) -> bytes:
    flat = [value for vector in vectors for value in vector]
    return struct.pack(f"<{len(flat)}f", *flat)
