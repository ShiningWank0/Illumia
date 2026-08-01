"""FastAPI RPC surface for the stateless Illumia ML sidecar."""

from __future__ import annotations

import base64
import json
from dataclasses import asdict
from typing import Any

import msgpack
from fastapi import FastAPI, HTTPException, Query, Request
from fastapi.concurrency import run_in_threadpool
from fastapi.responses import JSONResponse

from .backends import BackendManager
from .clustering import (
    cluster_assign,
    cluster_full,
    decode_f32,
    decode_medoids,
    params_from_request,
    validate_ids,
)


def _analysis_json(analysis: Any) -> dict[str, Any]:
    return {
        "model_version": analysis.model_version,
        "instances": [
            {
                "kind": instance.kind,
                "bbox": [round(value, 8) for value in instance.bbox],
                "det_conf": round(instance.det_conf, 8),
                "quality": {
                    "passed": instance.quality_passed,
                    "flags": list(instance.quality_flags),
                },
                "embedding": {
                    "dtype": "f32",
                    "dim": instance.embedding_dim,
                    "b64": base64.b64encode(instance.embedding).decode("ascii"),
                },
                "tags": [],
            }
            for instance in analysis.instances
        ],
    }


def _decode_request(content_type: str, body: bytes) -> dict[str, Any]:
    media_type = content_type.partition(";")[0].strip().lower()
    try:
        if media_type in {"application/msgpack", "application/x-msgpack"}:
            value = msgpack.unpackb(body, raw=False, strict_map_key=False)
        elif media_type == "application/json":
            value = json.loads(body)
        else:
            raise HTTPException(status_code=415, detail="unsupported content type")
    except (
        TypeError,
        ValueError,
        json.JSONDecodeError,
        msgpack.ExtraData,
        msgpack.FormatError,
        msgpack.StackError,
    ) as exc:
        raise HTTPException(status_code=422, detail="invalid request payload") from exc
    if not isinstance(value, dict):
        raise HTTPException(status_code=422, detail="request payload must be a map")
    return value


def _cluster(payload: dict[str, Any], manager: BackendManager) -> dict:
    mode = payload.get("mode")
    shape = payload.get("shape")
    if not isinstance(shape, list):
        raise ValueError("shape is required")
    vectors = decode_f32(payload.get("embeddings"), shape)
    ids = validate_ids(payload.get("ids"), vectors)
    raw_params = payload.get("params", {})
    if not isinstance(raw_params, dict):
        raise ValueError("params must be a map")
    params = params_from_request(raw_params, manager.backend.thresholds)
    common = {
        "rejections": payload.get("rejections"),
        "confirmed": payload.get("confirmed"),
    }
    if mode == "full":
        return cluster_full(ids, vectors, params, **common)
    if mode == "assign":
        dimension = int(shape[1])
        medoids = decode_medoids(payload.get("medoids"), dimension)
        return cluster_assign(ids, vectors, medoids, params, **common)
    raise ValueError("mode must be full or assign")


def create_app(*, manager: BackendManager | None = None) -> FastAPI:
    backend_manager = manager or BackendManager()
    app = FastAPI(title="illumia-ml", docs_url=None, redoc_url=None)
    app.state.backend_manager = backend_manager

    @app.exception_handler(Exception)
    async def sanitized_error_handler(_request: Request, _error: Exception) -> JSONResponse:
        # Do not log exception strings: model errors can contain local bundle paths.
        return JSONResponse(status_code=500, content={"detail": "ML operation failed"})

    @app.get("/ml/v1/health")
    async def health() -> dict[str, Any]:
        backend = backend_manager.backend
        return {
            "status": "ok",
            "backend": backend.name,
            "model_bundle": asdict(backend.model_bundle) if backend.model_bundle else None,
            "providers": list(backend.providers),
        }

    @app.post("/ml/v1/analyze")
    async def analyze(request: Request, tagger: bool = Query(default=False)) -> dict[str, Any]:
        media_type = request.headers.get("content-type", "").partition(";")[0].lower()
        if media_type != "application/octet-stream":
            raise HTTPException(
                status_code=415, detail="content type must be application/octet-stream"
            )
        body = await request.body()
        if not body:
            raise HTTPException(status_code=422, detail="image body must not be empty")
        try:
            result = await run_in_threadpool(backend_manager.analyze, body, tagger=tagger)
        except ValueError as exc:
            raise HTTPException(status_code=422, detail="invalid image data") from exc
        return _analysis_json(result)

    @app.post("/ml/v1/cluster")
    async def cluster(request: Request) -> dict:
        payload = _decode_request(request.headers.get("content-type", ""), await request.body())
        try:
            return await run_in_threadpool(_cluster, payload, backend_manager)
        except (TypeError, ValueError, OverflowError) as exc:
            raise HTTPException(status_code=422, detail="invalid clustering request") from exc

    return app
