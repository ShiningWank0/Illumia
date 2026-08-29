from __future__ import annotations

import hashlib
from pathlib import Path
from unittest.mock import Mock

import pytest
import yaml

import illumia_ml.bundles as bundles_module
from illumia_ml.backends import (
    MAX_EMBEDDING_DIMENSION,
    MAX_MODEL_INPUT_DIMENSION,
    MockBackend,
    OnnxBackend,
    select_backend,
)
from illumia_ml.bundles import discover_bundle
from illumia_ml.cli import main
from illumia_ml.models import BackendUnavailable


def _write_bundle(
    base: Path, directory: str, version: str, *, corrupt: bool = False
) -> tuple[Path, str]:
    bundle = base / directory
    bundle.mkdir()
    manifest = {
        "name": "test",
        "version": version,
        "models": {
            "detector": {
                "file": "detector.onnx",
                "input": {
                    "layout": "NCHW",
                    "dtype": "float32",
                    "size": [8, 8],
                    "mean": [0.0, 0.0, 0.0],
                    "std": [1.0, 1.0, 1.0],
                    "color": "RGB",
                },
                "outputs": ["boxes", "scores", "classes"],
                "classes": ["person", "head", "face"],
                "nms": {"iou": 0.45, "conf": 0.25},
            },
            "crop_encoder": {
                "file": "crop_encoder.onnx",
                "input": {
                    "layout": "NCHW",
                    "dtype": "float32",
                    "size": [8, 8],
                    "mean": [0.0, 0.0, 0.0],
                    "std": [1.0, 1.0, 1.0],
                    "color": "RGB",
                },
                "output": "embedding",
                "dim": 8,
                "normalize": "l2",
            },
        },
        "providers": ["CPUExecutionProvider"],
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
    checksum_bytes = "\n".join(lines).encode()
    (bundle / "checksums.sha256").write_bytes(checksum_bytes)
    return bundle, hashlib.sha256(checksum_bytes).hexdigest()


def test_checksum_failure_falls_back_to_mock(tmp_path: Path) -> None:
    _, digest = _write_bundle(tmp_path, "invalid-bundle", "1.0.0", corrupt=True)

    backend = select_backend(tmp_path, {digest})

    assert isinstance(backend, MockBackend)
    assert backend.model_bundle is None


def test_bundle_discovery_uses_semver_precedence_without_retaining_every_candidate(
    tmp_path: Path, monkeypatch
) -> None:
    _, prerelease = _write_bundle(tmp_path, "prerelease", "2.0.0-rc.1")
    _, release = _write_bundle(tmp_path, "release", "2.0.0")
    _, older = _write_bundle(tmp_path, "older", "1.10.0")
    fully_verified: list[str] = []
    verify = bundles_module._verify_checksum_entries

    def track_full_verification(root, *args, **kwargs):
        fully_verified.append(root.name)
        return verify(root, *args, **kwargs)

    monkeypatch.setattr(bundles_module, "_verify_checksum_entries", track_full_verification)

    bundle = discover_bundle(tmp_path, {prerelease, release, older})

    assert bundle is not None
    assert bundle.info.version == "2.0.0"
    assert fully_verified == ["release"]


def test_missing_onnxruntime_falls_back_to_mock(tmp_path: Path, monkeypatch) -> None:
    _, digest = _write_bundle(tmp_path, "valid", "1.0.0")
    monkeypatch.setattr(
        "illumia_ml.backends.importlib.import_module", Mock(side_effect=ImportError)
    )

    backend = select_backend(tmp_path, {digest})

    assert isinstance(backend, MockBackend)
    assert backend.model_bundle is None


def test_self_signed_bundle_is_rejected_without_external_digest(tmp_path: Path) -> None:
    _write_bundle(tmp_path, "self-signed", "1.0.0")

    assert discover_bundle(tmp_path, set()) is None
    assert isinstance(select_backend(tmp_path, set()), MockBackend)


def test_untrusted_manifest_is_not_parsed(tmp_path: Path, monkeypatch) -> None:
    _write_bundle(tmp_path, "untrusted", "1.0.0")
    safe_load = Mock(side_effect=AssertionError("untrusted YAML must not be parsed"))
    monkeypatch.setattr("illumia_ml.bundles.yaml.safe_load", safe_load)

    assert discover_bundle(tmp_path, {"0" * 64}) is None
    safe_load.assert_not_called()


def test_trusted_recursive_manifest_fails_closed_per_candidate(tmp_path: Path) -> None:
    bundle, _ = _write_bundle(tmp_path, "recursive", "1.0.0")
    manifest = ("name: " + "[" * 2000 + "x" + "]" * 2000).encode()
    (bundle / "manifest.yaml").write_bytes(manifest)
    files = {
        name: (bundle / name).read_bytes()
        for name in [
            "manifest.yaml",
            "thresholds.yaml",
            "detector.onnx",
            "crop_encoder.onnx",
        ]
    }
    checksums = "\n".join(
        f"{hashlib.sha256(content).hexdigest()}  {name}" for name, content in files.items()
    ).encode()
    (bundle / "checksums.sha256").write_bytes(checksums)

    assert discover_bundle(tmp_path, {hashlib.sha256(checksums).hexdigest()}) is None


def test_verified_bundle_total_size_is_bounded(tmp_path: Path, monkeypatch) -> None:
    _, digest = _write_bundle(tmp_path, "too-large", "1.0.0")
    monkeypatch.setattr("illumia_ml.bundles._MAX_BUNDLE_BYTES", 16)

    assert discover_bundle(tmp_path, {digest}) is None


def test_onnx_runtime_receives_verified_bytes_not_reopened_paths(tmp_path: Path) -> None:
    bundle_path, digest = _write_bundle(tmp_path, "valid", "1.0.0")
    bundle = discover_bundle(tmp_path, {digest})
    assert bundle is not None
    runtime = Mock()
    runtime.get_available_providers.return_value = ["CPUExecutionProvider"]
    runtime.InferenceSession.side_effect = [Mock(), Mock()]
    backend = OnnxBackend(bundle, runtime)

    (bundle_path / "detector.onnx").write_bytes(b"tampered-after-verification")
    (bundle_path / "crop_encoder.onnx").write_bytes(b"tampered-after-verification")
    backend._sessions()

    assert runtime.InferenceSession.call_args_list[0].args[0] == b"detector"
    assert runtime.InferenceSession.call_args_list[1].args[0] == b"encoder"
    assert not backend.bundle.verified_files


def test_manifest_resource_dimensions_are_bounded(tmp_path: Path) -> None:
    for index, (field, value) in enumerate(
        [
            (("detector", "input", "size"), [MAX_MODEL_INPUT_DIMENSION + 1, 1]),
            (("detector", "input", "size"), [1.5, 1]),
            (("crop_encoder", "dim"), MAX_EMBEDDING_DIMENSION + 1),
        ]
    ):
        bundle_path, digest = _write_bundle(tmp_path, f"invalid-{index}", "1.0.0")
        manifest = yaml.safe_load((bundle_path / "manifest.yaml").read_bytes())
        target = manifest["models"]
        for component in field[:-1]:
            target = target[component]
        target[field[-1]] = value
        files = {
            name: (bundle_path / name).read_bytes()
            for name in ["thresholds.yaml", "detector.onnx", "crop_encoder.onnx"]
        }
        files["manifest.yaml"] = yaml.safe_dump(manifest).encode()
        (bundle_path / "manifest.yaml").write_bytes(files["manifest.yaml"])
        checksums = "\n".join(
            f"{hashlib.sha256(content).hexdigest()}  {name}" for name, content in files.items()
        ).encode()
        (bundle_path / "checksums.sha256").write_bytes(checksums)
        trusted_digest = hashlib.sha256(checksums).hexdigest()

        bundle = discover_bundle(tmp_path, {trusted_digest})
        assert bundle is not None
        runtime = Mock()
        runtime.get_available_providers.return_value = ["CPUExecutionProvider"]
        with pytest.raises(BackendUnavailable):
            OnnxBackend(bundle, runtime)


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
