"""Model bundle discovery and integrity validation."""

from __future__ import annotations

import hashlib
import os
import re
import stat
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path
from types import MappingProxyType
from typing import Any

import yaml

from .models import ModelBundleInfo, QualityThresholds

DEFAULT_MODEL_DIR = Path("models")
TRUSTED_DIGESTS_ENV = "ILLUMIA_TRUSTED_MODEL_DIGESTS"
_SHA256_RE = re.compile(r"^[0-9a-fA-F]{64}$")
_MAX_CHECKSUM_BYTES = 64 * 1024
_MAX_YAML_BYTES = 1024 * 1024
_MAX_CHECKSUM_ENTRIES = 64
_MAX_BUNDLE_BYTES = 512 * 1024 * 1024
_READ_CHUNK_BYTES = 1024 * 1024


@dataclass(frozen=True, slots=True)
class Bundle:
    root: Path
    manifest: dict[str, Any]
    thresholds: QualityThresholds
    info: ModelBundleInfo
    verified_files: Mapping[Path, bytes]

    def verified_bytes(self, relative: str) -> bytes:
        path = _relative_path(relative)
        try:
            return self.verified_files[path]
        except KeyError as exc:
            raise ValueError("model file was not retained after verification") from exc

    def release_verified_bytes(self) -> None:
        """Release model payloads after the runtime has synchronously created sessions."""
        object.__setattr__(self, "verified_files", MappingProxyType({}))


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


def _read_yaml_bytes(raw: bytes) -> dict[str, Any]:
    if len(raw) > _MAX_YAML_BYTES:
        raise ValueError("YAML file is too large")
    loaded = yaml.safe_load(raw.decode("utf-8"))
    return _mapping(loaded)


def _read_small_file(path: Path, limit: int) -> bytes:
    with path.open("rb") as handle:
        raw = handle.read(limit + 1)
    if len(raw) > limit:
        raise ValueError("bundle metadata file is too large")
    return raw


def _relative_path(value: str) -> Path:
    relative = Path(value.lstrip("*"))
    if not value or relative == Path(".") or relative.is_absolute() or ".." in relative.parts:
        raise ValueError("invalid bundle path")
    return relative


def _trusted_digests(configured: set[str] | frozenset[str] | None) -> frozenset[str]:
    values = (
        os.environ.get(TRUSTED_DIGESTS_ENV, "").split(",") if configured is None else configured
    )
    result = frozenset(value.strip().lower() for value in values if value.strip())
    if any(not _SHA256_RE.fullmatch(value) for value in result):
        raise ValueError("invalid trusted model digest")
    return result


def _parse_checksums(root: Path, trusted_digests: frozenset[str]) -> tuple[dict[Path, str], str]:
    checksum_path = root / "checksums.sha256"
    raw = _read_small_file(checksum_path, _MAX_CHECKSUM_BYTES)
    bundle_sha256 = hashlib.sha256(raw).hexdigest()
    if bundle_sha256 not in trusted_digests:
        raise ValueError("model bundle digest is not trusted")
    entries: dict[Path, str] = {}
    for line in raw.decode("utf-8").splitlines():
        line = line.strip()
        if not line:
            continue
        fields = line.split(maxsplit=1)
        if len(fields) != 2 or not _SHA256_RE.fullmatch(fields[0]):
            raise ValueError("invalid checksum manifest")
        relative = _relative_path(fields[1])
        if relative in entries:
            raise ValueError("invalid checksum target")
        entries[relative] = fields[0].lower()
        if len(entries) > _MAX_CHECKSUM_ENTRIES:
            raise ValueError("too many checksum entries")
    if not entries:
        raise ValueError("empty checksum manifest")
    return entries, bundle_sha256


def _read_verified_file(path: Path, *, retain: bool, remaining: int) -> tuple[bytes, str, int]:
    flags = os.O_RDONLY
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    descriptor = os.open(path, flags)
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_size < 0:
            raise ValueError("checksummed target is not a regular file")
        if metadata.st_size > remaining:
            raise ValueError("model bundle is too large")
        digest = hashlib.sha256()
        retained = bytearray() if retain else None
        consumed = 0
        with os.fdopen(descriptor, "rb", closefd=False) as handle:
            while chunk := handle.read(_READ_CHUNK_BYTES):
                consumed += len(chunk)
                if consumed > remaining:
                    raise ValueError("model bundle is too large")
                digest.update(chunk)
                if retained is not None:
                    retained.extend(chunk)
        return bytes(retained or b""), digest.hexdigest(), consumed
    finally:
        os.close(descriptor)


def _verify_checksum_entries(
    root: Path,
    entries: dict[Path, str],
    bundle_sha256: str,
    required: set[Path],
    *,
    preverified: dict[Path, bytes] | None = None,
    checked_bytes: int = 0,
) -> tuple[str, Mapping[Path, bytes]]:
    if not required.issubset(entries):
        raise ValueError("required files are not checksummed")
    retained = dict(preverified or {})
    for relative, expected in entries.items():
        if relative in retained:
            continue
        target = root / relative
        if (
            target.is_symlink()
            or not target.is_file()
            or not target.resolve().is_relative_to(root.resolve())
        ):
            raise ValueError("checksummed file is missing")
        content, actual, consumed = _read_verified_file(
            target,
            retain=relative in required,
            remaining=_MAX_BUNDLE_BYTES - checked_bytes,
        )
        checked_bytes += consumed
        if actual != expected:
            raise ValueError("checksum mismatch")
        if relative in required:
            retained[relative] = content
    return bundle_sha256, MappingProxyType(retained)


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


def load_bundle(
    root: Path,
    trusted_digests: set[str] | frozenset[str] | None = None,
) -> Bundle:
    trusted = _trusted_digests(trusted_digests)
    if not trusted:
        raise ValueError("no trusted model digests are configured")
    entries, bundle_sha256 = _parse_checksums(root, trusted)
    manifest_path = Path("manifest.yaml")
    if manifest_path not in entries:
        raise ValueError("required files are not checksummed")
    manifest_bytes, manifest_digest, manifest_size = _read_verified_file(
        root / manifest_path,
        retain=True,
        remaining=_MAX_BUNDLE_BYTES,
    )
    if manifest_digest != entries[manifest_path]:
        raise ValueError("checksum mismatch")
    manifest = _read_yaml_bytes(manifest_bytes)
    name = manifest.get("name")
    version = manifest.get("version")
    models = _mapping(manifest.get("models"))
    detector = _mapping(models.get("detector"))
    encoder = _mapping(models.get("crop_encoder"))
    if (
        not isinstance(name, str)
        or not 0 < len(name) <= 256
        or not isinstance(version, str)
        or not 0 < len(version) <= 128
    ):
        raise ValueError("invalid bundle identity")
    _version_key(version)
    detector_file = detector.get("file")
    encoder_file = encoder.get("file")
    if not isinstance(detector_file, str) or not isinstance(encoder_file, str):
        raise ValueError("invalid model file declaration")
    detector_path = _relative_path(detector_file)
    encoder_path = _relative_path(encoder_file)
    required = {
        Path("manifest.yaml"),
        Path("thresholds.yaml"),
        detector_path,
        encoder_path,
    }
    bundle_sha256, verified_files = _verify_checksum_entries(
        root,
        entries,
        bundle_sha256,
        required,
        preverified={manifest_path: manifest_bytes},
        checked_bytes=manifest_size,
    )
    manifest = _read_yaml_bytes(verified_files[manifest_path])
    name = manifest.get("name")
    version = manifest.get("version")
    if (
        not isinstance(name, str)
        or not 0 < len(name) <= 256
        or not isinstance(version, str)
        or not 0 < len(version) <= 128
    ):
        raise ValueError("invalid bundle identity")
    _version_key(version)
    verified_models = _mapping(manifest.get("models"))
    verified_detector = _mapping(verified_models.get("detector"))
    verified_encoder = _mapping(verified_models.get("crop_encoder"))
    if (
        verified_detector.get("file") != detector_file
        or verified_encoder.get("file") != encoder_file
    ):
        raise ValueError("model declaration changed during verification")
    threshold_values = _thresholds(_read_yaml_bytes(verified_files[Path("thresholds.yaml")]))
    return Bundle(
        root=root,
        manifest=manifest,
        thresholds=threshold_values,
        info=ModelBundleInfo(name=name, version=version, sha256=bundle_sha256),
        verified_files=verified_files,
    )


def _trusted_candidate_version(root: Path, trusted_digests: frozenset[str]) -> tuple:
    """Read only trusted manifest bytes while ranking bundle candidates."""
    entries, _ = _parse_checksums(root, trusted_digests)
    manifest_path = Path("manifest.yaml")
    if manifest_path not in entries:
        raise ValueError("required files are not checksummed")
    manifest_bytes, manifest_digest, _ = _read_verified_file(
        root / manifest_path,
        retain=True,
        remaining=_MAX_BUNDLE_BYTES,
    )
    if manifest_digest != entries[manifest_path]:
        raise ValueError("checksum mismatch")
    manifest = _read_yaml_bytes(manifest_bytes)
    name = manifest.get("name")
    version = manifest.get("version")
    if (
        not isinstance(name, str)
        or not 0 < len(name) <= 256
        or not isinstance(version, str)
        or not 0 < len(version) <= 128
    ):
        raise ValueError("invalid bundle identity")
    return _version_key(version)


def discover_bundle(
    model_dir: Path | None = None,
    trusted_digests: set[str] | frozenset[str] | None = None,
) -> Bundle | None:
    """Select the greatest valid manifest version, ignoring unusable bundles."""
    base = model_dir or Path(os.environ.get("ILLUMIA_MODEL_DIR", DEFAULT_MODEL_DIR))
    if not base.is_dir():
        return None
    try:
        trusted = _trusted_digests(trusted_digests)
    except ValueError:
        return None
    if not trusted:
        return None
    candidates: list[tuple[tuple, Path]] = []
    for directory in base.iterdir():
        if directory.is_symlink() or not directory.is_dir():
            continue
        try:
            candidates.append((_trusted_candidate_version(directory, trusted), directory))
        except (OSError, UnicodeError, ValueError, RecursionError, yaml.YAMLError):
            continue
    for _, directory in sorted(candidates, reverse=True):
        try:
            return load_bundle(directory, trusted)
        except (OSError, UnicodeError, ValueError, RecursionError, yaml.YAMLError):
            continue
    return None
