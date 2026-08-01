from __future__ import annotations

from fastapi.testclient import TestClient


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
