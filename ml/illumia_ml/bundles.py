"""Model bundle discovery and integrity validation."""

from __future__ import annotations

import hashlib
import os
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import yaml

from .models import ModelBundleInfo, QualityThresholds

DEFAULT_MODEL_DIR = Path("models")
_SHA256_RE = re.compile(r"^[0-9a-fA-F]{64}$")


@dataclass(frozen=True, slots=True)
class Bundle:
    root: Path
    manifest: dict[str, Any]
    thresholds: QualityThresholds
    info: ModelBundleInfo


def _mapping(value: object) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ValueError("expected a mapping")
    return value


def _version_key(version: str) -> tuple[int, int, int, int, tuple[tuple[int, int | str], ...]]:
    """Return a SemVer precedence key, excluding build metadata."""
    match = re.fullmatch(
        r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)"
        r"(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+[0-9A-Za-z.-]+)?",
        version,
    )
    if match is None:
        raise ValueError("bundle version must be semver")
    prerelease = match.group(4)
    prerelease_key = (
        tuple(
            (0, int(part)) if part.isdigit() else (1, part.lower())
            for part in prerelease.split(".")
        )
        if prerelease
        else ()
    )
    return (
        int(match.group(1)),
        int(match.group(2)),
        int(match.group(3)),
        int(prerelease is None),
        prerelease_key,
    )


def _read_yaml(path: Path) -> dict[str, Any]:
    loaded = yaml.safe_load(path.read_text(encoding="utf-8"))
    return _mapping(loaded)


def _parse_checksums(root: Path) -> tuple[dict[Path, str], str]:
    checksum_path = root / "checksums.sha256"
    raw = checksum_path.read_bytes()
    entries: dict[Path, str] = {}
    for line in raw.decode("utf-8").splitlines():
        line = line.strip()
        if not line:
            continue
        fields = line.split(maxsplit=1)
        if len(fields) != 2 or not _SHA256_RE.fullmatch(fields[0]):
            raise ValueError("invalid checksum manifest")
        relative = Path(fields[1].lstrip("*"))
        if relative.is_absolute() or ".." in relative.parts or relative in entries:
            raise ValueError("invalid checksum target")
        entries[relative] = fields[0].lower()
    if not entries:
        raise ValueError("empty checksum manifest")
    return entries, hashlib.sha256(raw).hexdigest()


def _verify_checksums(root: Path, required: set[Path]) -> str:
    entries, bundle_sha256 = _parse_checksums(root)
    if not required.issubset(entries):
        raise ValueError("required files are not checksummed")
    for relative, expected in entries.items():
        target = root / relative
        if (
            target.is_symlink()
            or not target.is_file()
            or not target.resolve().is_relative_to(root.resolve())
        ):
            raise ValueError("checksummed file is missing")
        actual = hashlib.sha256(target.read_bytes()).hexdigest()
        if actual != expected:
            raise ValueError("checksum mismatch")
    return bundle_sha256


def _thresholds(data: dict[str, Any]) -> QualityThresholds:
    quality = _mapping(data.get("quality_gate", {}))
    result = QualityThresholds(
        tau_high=float(data.get("tau_high", 0.82)),
        tau_low=float(data.get("tau_low", 0.55)),
        min_cluster_size=int(data.get("min_cluster_size", 3)),
        min_face_size=int(quality.get("min_face_size", 48)),
        min_det_conf=float(quality.get("min_det_conf", 0.5)),
        min_visibility=float(quality.get("min_visibility", 0.6)),
    )
    if not 0.0 <= result.tau_low <= result.tau_high <= 1.0:
        raise ValueError("invalid clustering thresholds")
    if (
        result.min_cluster_size < 1
        or result.min_face_size < 1
        or not 0.0 <= result.min_det_conf <= 1.0
        or not 0.0 <= result.min_visibility <= 1.0
    ):
        raise ValueError("invalid quality thresholds")
    return result


def load_bundle(root: Path) -> Bundle:
    manifest = _read_yaml(root / "manifest.yaml")
    name = manifest.get("name")
    version = manifest.get("version")
    models = _mapping(manifest.get("models"))
    detector = _mapping(models.get("detector"))
    encoder = _mapping(models.get("crop_encoder"))
    if not isinstance(name, str) or not name or not isinstance(version, str) or not version:
        raise ValueError("invalid bundle identity")
    _version_key(version)
    detector_file = detector.get("file")
    encoder_file = encoder.get("file")
    if not isinstance(detector_file, str) or not isinstance(encoder_file, str):
        raise ValueError("invalid model file declaration")
    required = {
        Path("manifest.yaml"),
        Path("thresholds.yaml"),
        Path(detector_file),
        Path(encoder_file),
    }
    bundle_sha256 = _verify_checksums(root, required)
    threshold_values = _thresholds(_read_yaml(root / "thresholds.yaml"))
    return Bundle(
        root=root,
        manifest=manifest,
        thresholds=threshold_values,
        info=ModelBundleInfo(name=name, version=version, sha256=bundle_sha256),
    )


def discover_bundle(model_dir: Path | None = None) -> Bundle | None:
    """Select the greatest valid manifest version, ignoring unusable bundles."""
    base = model_dir or Path(os.environ.get("ILLUMIA_MODEL_DIR", DEFAULT_MODEL_DIR))
    if not base.is_dir():
        return None
    candidates: list[Bundle] = []
    for directory in base.iterdir():
        if directory.is_symlink() or not directory.is_dir():
            continue
        try:
            candidates.append(load_bundle(directory))
        except (OSError, UnicodeError, ValueError, yaml.YAMLError):
            continue
    if not candidates:
        return None
    return max(candidates, key=lambda bundle: _version_key(bundle.info.version))
