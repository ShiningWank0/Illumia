from __future__ import annotations

import hashlib
from pathlib import Path
from unittest.mock import Mock

import yaml

from illumia_ml.backends import MockBackend, select_backend
from illumia_ml.bundles import discover_bundle
from illumia_ml.cli import main


def _write_bundle(base: Path, directory: str, version: str, *, corrupt: bool = False) -> Path:
    bundle = base / directory
    bundle.mkdir()
    manifest = {
        "name": "test",
        "version": version,
        "models": {
            "detector": {"file": "detector.onnx"},
            "crop_encoder": {"file": "crop_encoder.onnx"},
        },
    }
    thresholds = {
        "tau_high": 0.8,
        "tau_low": 0.5,
        "min_cluster_size": 2,
        "quality_gate": {
            "min_face_size": 32,
            "min_det_conf": 0.4,
            "min_visibility": 0.5,
        },
    }
    files = {
        "manifest.yaml": yaml.safe_dump(manifest).encode(),
        "thresholds.yaml": yaml.safe_dump(thresholds).encode(),
        "detector.onnx": b"detector",
        "crop_encoder.onnx": b"encoder",
    }
    for name, content in files.items():
        (bundle / name).write_bytes(content)
    lines = [f"{hashlib.sha256(content).hexdigest()}  {name}" for name, content in files.items()]
    if corrupt:
        lines[-1] = f"{'0' * 64}  crop_encoder.onnx"
    (bundle / "checksums.sha256").write_text("\n".join(lines), encoding="utf-8")
    return bundle


def test_checksum_failure_falls_back_to_mock(tmp_path: Path) -> None:
    _write_bundle(tmp_path, "invalid-bundle", "1.0.0", corrupt=True)

    backend = select_backend(tmp_path)

    assert isinstance(backend, MockBackend)
    assert backend.model_bundle is None


def test_bundle_discovery_uses_semver_precedence(tmp_path: Path) -> None:
    _write_bundle(tmp_path, "prerelease", "2.0.0-rc.1")
    _write_bundle(tmp_path, "release", "2.0.0")
    _write_bundle(tmp_path, "older", "1.10.0")

    bundle = discover_bundle(tmp_path)

    assert bundle is not None
    assert bundle.info.version == "2.0.0"


def test_missing_onnxruntime_falls_back_to_mock(tmp_path: Path, monkeypatch) -> None:
    _write_bundle(tmp_path, "valid", "1.0.0")
    monkeypatch.setattr(
        "illumia_ml.backends.importlib.import_module", Mock(side_effect=ImportError)
    )

    backend = select_backend(tmp_path)

    assert isinstance(backend, MockBackend)
    assert backend.model_bundle is None


def test_cli_starts_uvicorn_on_uds_without_access_log(monkeypatch) -> None:
    run = Mock()
    monkeypatch.setattr("illumia_ml.cli.uvicorn.run", run)

    main(["--socket", "/tmp/illumia-test.sock"])

    run.assert_called_once_with(
        "illumia_ml.app:create_app",
        factory=True,
        uds="/tmp/illumia-test.sock",
        access_log=False,
    )
