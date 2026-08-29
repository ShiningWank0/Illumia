from __future__ import annotations

import numpy as np
from fastapi.testclient import TestClient

from illumia_ml import clustering
from illumia_ml.backends import MAX_ANALYSIS_INSTANCES, _nms


def test_full_clustering_separates_groups_and_leaves_outlier(client: TestClient) -> None:
    payload = {
        "mode": "full",
        "params": {"tau_high": 0.95, "tau_low": 0.8, "min_cluster_size": 2},
        "embeddings": [
            [1.0, 0.0],
            [0.99, 0.1],
            [0.0, 1.0],
            [0.1, 0.99],
            [-1.0, 0.0],
        ],
        "shape": [5, 2],
        "ids": ["a1", "a2", "b1", "b2", "outlier"],
    }

    response = client.post("/ml/v1/cluster", json=payload)

    assert response.status_code == 200
    body = response.json()
    groups = {frozenset(cluster["member_ids"]) for cluster in body["new_clusters"]}
    assert groups == {frozenset({"a1", "a2"}), frozenset({"b1", "b2"})}
    assignments = {item["id"]: item for item in body["assignments"]}
    assert assignments["a1"]["cluster"] == assignments["a2"]["cluster"]
    assert assignments["b1"]["cluster"] == assignments["b2"]["cluster"]
    assert assignments["a1"]["cluster"] != assignments["b1"]["cluster"]
    assert assignments["outlier"]["cluster"] is None
    assert assignments["outlier"]["state"] == "unassigned"


def test_small_full_cluster_is_unassigned(client: TestClient) -> None:
    payload = {
        "mode": "full",
        "params": {"tau_high": 0.9, "tau_low": 0.5, "min_cluster_size": 3},
        "embeddings": [[1.0, 0.0], [0.99, 0.01]],
        "shape": [2, 2],
        "ids": ["one", "two"],
    }

    response = client.post("/ml/v1/cluster", json=payload)

    assert response.status_code == 200
    assert response.json()["new_clusters"] == []
    assert {item["state"] for item in response.json()["assignments"]} == {"unassigned"}


def test_assign_uses_medoids_rejections_and_confirmed(client: TestClient) -> None:
    payload = {
        "mode": "assign",
        "params": {"tau_high": 0.9, "tau_low": 0.6},
        "embeddings": [[0.99, 0.05], [0.05, 0.99], [0.7, 0.7], [-0.7, 0.7]],
        "shape": [4, 2],
        "ids": ["a", "b", "candidate", "confirmed"],
        "medoids": {"left": [1.0, 0.0], "right": [0.0, 1.0]},
        "rejections": [["b", "right"]],
        "confirmed": [["confirmed", "left"]],
    }

    response = client.post("/ml/v1/cluster", json=payload)

    assert response.status_code == 200
    assignments = {item["id"]: item for item in response.json()["assignments"]}
    assert assignments["a"]["cluster"] == "left"
    assert assignments["a"]["state"] == "auto"
    assert assignments["b"]["cluster"] is None
    assert assignments["b"]["state"] == "unassigned"
    assert assignments["candidate"]["cluster"] in {"left", "right"}
    assert assignments["candidate"]["state"] == "candidate"
    assert "confirmed" not in assignments


def test_full_omits_confirmed_ids(client: TestClient) -> None:
    payload = {
        "mode": "full",
        "params": {"tau_high": 0.9, "tau_low": 0.5, "min_cluster_size": 2},
        "embeddings": [[1.0, 0.0], [0.99, 0.01], [0.98, 0.02]],
        "shape": [3, 2],
        "ids": ["fixed", "one", "two"],
        "confirmed": {"fixed": "existing"},
    }

    response = client.post("/ml/v1/cluster", json=payload)

    assert response.status_code == 200
    assert {item["id"] for item in response.json()["assignments"]} == {"one", "two"}


def test_full_clustering_similarity_work_is_quadratic(monkeypatch) -> None:
    groups = 32
    rows = groups * 2
    calls = 0
    original = clustering._similarity

    def counted(left, right):
        nonlocal calls
        calls += 1
        return original(left, right)

    monkeypatch.setattr(clustering, "_similarity", counted)
    vectors = []
    for group in range(groups):
        vector = [0.0] * groups
        vector[group] = 1.0
        vectors.extend([vector, vector.copy()])
    clustering.cluster_full(
        [str(index) for index in range(rows)],
        vectors,
        clustering.ClusterParams(tau_high=0.99, tau_low=0.99, min_cluster_size=2),
    )
    assert calls < 3 * rows * rows


def test_cluster_rows_over_safety_budget_are_rejected(client: TestClient) -> None:
    rows = clustering.MAX_CLUSTER_ROWS + 1
    response = client.post(
        "/ml/v1/cluster",
        json={
            "mode": "full",
            "params": {},
            "embeddings": [[1.0] for _ in range(rows)],
            "shape": [rows, 1],
            "ids": [str(index) for index in range(rows)],
        },
    )
    assert response.status_code == 422


def test_assign_rejects_too_many_medoids(client: TestClient) -> None:
    response = client.post(
        "/ml/v1/cluster",
        json={
            "mode": "assign",
            "params": {},
            "embeddings": [[1.0]],
            "shape": [1, 1],
            "ids": ["new"],
            "medoids": {
                f"cluster-{index}": [1.0] for index in range(clustering.MAX_CLUSTER_ROWS + 1)
            },
        },
    )

    assert response.status_code == 422


def test_nms_retained_output_is_bounded() -> None:
    rows = clustering.MAX_CLUSTER_ROWS
    boxes = np.zeros((rows, 4), dtype=np.float32)
    scores = np.linspace(1.0, 0.1, rows, dtype=np.float32)
    classes = np.zeros(rows, dtype=np.int64)

    retained = _nms(boxes, scores, classes, 0.5)

    assert len(retained) == MAX_ANALYSIS_INSTANCES
