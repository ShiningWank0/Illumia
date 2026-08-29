from __future__ import annotations

import base64
import math
import struct

import msgpack
import pytest
from fastapi.testclient import TestClient

from illumia_ml.app import MAX_ANALYZE_BODY_BYTES, MAX_CLUSTER_BODY_BYTES


def test_health_reports_mock_backend(client: TestClient) -> None:
    response = client.get("/ml/v1/health")

    assert response.status_code == 200
    assert response.json() == {
        "status": "ok",
        "backend": "mock",
        "model_bundle": None,
        "providers": [],
    }


def test_analyze_is_deterministic_and_matches_schema(
    client: TestClient, image_bytes: bytes
) -> None:
    headers = {"Content-Type": "application/octet-stream"}
    first = client.post("/ml/v1/analyze?tagger=false", content=image_bytes, headers=headers)
    second = client.post("/ml/v1/analyze?tagger=false", content=image_bytes, headers=headers)

    assert first.status_code == 200
    assert first.json() == second.json()
    body = first.json()
    assert body["model_version"] == "mock-v1"
    assert 1 <= len(body["instances"]) <= 2
    for instance in body["instances"]:
        assert instance["kind"] == "face"
        assert len(instance["bbox"]) == 4
        x, y, width, height = instance["bbox"]
        assert 0.0 <= x <= 1.0
        assert 0.0 <= y <= 1.0
        assert 0.0 < width <= 1.0
        assert 0.0 < height <= 1.0
        assert x + width <= 1.00000001
        assert y + height <= 1.00000001
        assert 0.0 <= instance["det_conf"] <= 1.0
        assert set(instance["quality"]) == {"passed", "flags"}
        assert instance["embedding"]["dtype"] == "f32"
        assert instance["embedding"]["dim"] == 768
        raw = base64.b64decode(instance["embedding"]["b64"], validate=True)
        assert len(raw) == 768 * 4
        values = struct.unpack("<768f", raw)
        assert math.sqrt(sum(value * value for value in values)) == pytest.approx(1.0)
        assert instance["tags"] == []


def test_analyze_rejects_wrong_media_type(client: TestClient, image_bytes: bytes) -> None:
    response = client.post("/ml/v1/analyze", content=image_bytes)

    assert response.status_code == 415


@pytest.mark.parametrize(
    ("path", "content_type", "limit"),
    [
        ("/ml/v1/analyze", "application/octet-stream", MAX_ANALYZE_BODY_BYTES),
        ("/ml/v1/cluster", "application/json", MAX_CLUSTER_BODY_BYTES),
    ],
)
def test_request_body_rejects_oversized_content_length_before_reading(
    client: TestClient, path: str, content_type: str, limit: int
) -> None:
    response = client.post(
        path,
        content=b"x",
        headers={"Content-Type": content_type, "Content-Length": str(limit + 1)},
    )

    assert response.status_code == 413


def test_streamed_request_body_is_bounded_without_content_length(
    client: TestClient, monkeypatch
) -> None:
    monkeypatch.setattr("illumia_ml.app.MAX_CLUSTER_BODY_BYTES", 4)

    response = client.post(
        "/ml/v1/cluster",
        content=(chunk for chunk in [b"abc", b"de"]),
        headers={"Content-Type": "application/json"},
    )

    assert response.status_code == 413


def test_cluster_accepts_msgpack(client: TestClient) -> None:
    payload = {
        "mode": "assign",
        "params": {"tau_high": 0.9, "tau_low": 0.6},
        "embeddings": struct.pack("<2f", 1.0, 0.0),
        "shape": [1, 2],
        "ids": ["new"],
        "medoids": {"known": struct.pack("<2f", 1.0, 0.0)},
    }

    response = client.post(
        "/ml/v1/cluster",
        content=msgpack.packb(payload, use_bin_type=True),
        headers={"Content-Type": "application/msgpack"},
    )

    assert response.status_code == 200
    assert response.json()["assignments"] == [
        {"id": "new", "cluster": "known", "state": "auto", "similarity": 1.0}
    ]
