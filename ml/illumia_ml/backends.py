"""Deterministic mock and manifest-driven ONNX inference backends."""

from __future__ import annotations

import hashlib
import importlib
import io
import math
import random
import struct
import threading
from pathlib import Path
from typing import Any, Literal, cast

import numpy as np
from PIL import Image

from .bundles import Bundle, discover_bundle
from .models import (
    Analysis,
    Backend,
    BackendUnavailable,
    Instance,
    ModelBundleInfo,
    QualityThresholds,
)

MOCK_MODEL_VERSION = "mock-v1"
EMBEDDING_DIM = 768
MAX_MODEL_INPUT_DIMENSION = 4096
MAX_MODEL_INPUT_PIXELS = 16_777_216
MAX_EMBEDDING_DIMENSION = 4096
MAX_DETECTOR_CANDIDATES = 4096
MAX_ANALYSIS_INSTANCES = 256


def _quality(
    *,
    crop_width: float,
    crop_height: float,
    confidence: float,
    visibility: float,
    thresholds: QualityThresholds,
) -> tuple[bool, tuple[str, ...]]:
    flags: list[str] = []
    if min(crop_width, crop_height) < thresholds.min_face_size:
        flags.append("face_too_small")
    if confidence < thresholds.min_det_conf:
        flags.append("low_det_conf")
    if visibility < thresholds.min_visibility:
        flags.append("low_visibility")
    return not flags, tuple(flags)


def _embedding_bytes(values: list[float]) -> bytes:
    norm = math.sqrt(sum(value * value for value in values))
    normalized = [value / norm for value in values]
    return struct.pack(f"<{len(normalized)}f", *normalized)


class MockBackend:
    """Model-free deterministic backend suitable for development and tests."""

    name: Literal["mock"] = "mock"
    model_bundle: ModelBundleInfo | None = None
    providers: tuple[str, ...] = ()

    def __init__(self, thresholds: QualityThresholds | None = None) -> None:
        self.thresholds = thresholds or QualityThresholds()

    def analyze(self, image_bytes: bytes, *, tagger: bool = False) -> Analysis:
        del tagger
        try:
            with Image.open(io.BytesIO(image_bytes)) as image:
                width, height = image.size
                image.load()
        except (OSError, ValueError) as exc:
            raise ValueError("request body is not a supported image") from exc
        if width <= 0 or height <= 0:
            raise ValueError("image dimensions must be positive")

        digest = hashlib.sha256(image_bytes).digest()
        count = 1 + ((width + height + digest[0]) & 1)
        instances: list[Instance] = []
        for index in range(count):
            box_width = min(0.56, 0.24 + digest[1 + index] / 1024.0)
            box_height = min(0.62, 0.27 + digest[3 + index] / 1024.0)
            x_span = max(0.0, 1.0 - box_width)
            y_span = max(0.0, 1.0 - box_height)
            x = (digest[5 + index] / 255.0) * x_span
            y = (digest[7 + index] / 255.0) * y_span
            confidence = 0.65 + (digest[9 + index] / 255.0) * 0.34
            seed = int.from_bytes(
                hashlib.sha256(image_bytes + index.to_bytes(2, "little")).digest()[:8],
                "little",
            )
            generator = random.Random(seed)
            embedding = _embedding_bytes(
                [generator.uniform(-1.0, 1.0) for _ in range(EMBEDDING_DIM)]
            )
            passed, flags = _quality(
                crop_width=box_width * width,
                crop_height=box_height * height,
                confidence=confidence,
                visibility=1.0,
                thresholds=self.thresholds,
            )
            instances.append(
                Instance(
                    kind="face",
                    bbox=(x, y, box_width, box_height),
                    det_conf=confidence,
                    quality_passed=passed,
                    quality_flags=flags,
                    embedding=embedding,
                    embedding_dim=EMBEDDING_DIM,
                )
            )
        return Analysis(model_version=MOCK_MODEL_VERSION, instances=tuple(instances))


def _as_mapping(value: object) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise BackendUnavailable("invalid model manifest")
    return value


def _input_config(model: dict[str, Any]) -> dict[str, Any]:
    config = _as_mapping(model.get("input"))
    size = config.get("size")
    mean = config.get("mean")
    std = config.get("std")
    try:
        dimensions = (
            [int(value) for value in size]
            if isinstance(size, list)
            and all(isinstance(value, int) and not isinstance(value, bool) for value in size)
            else []
        )
        means = [float(value) for value in mean] if isinstance(mean, list) else []
        standard_deviations = [float(value) for value in std] if isinstance(std, list) else []
    except (TypeError, ValueError, OverflowError) as exc:
        raise BackendUnavailable("invalid preprocessing manifest") from exc
    if (
        config.get("layout") not in {"NCHW", "NHWC"}
        or config.get("dtype") != "float32"
        or config.get("color") not in {"RGB", "BGR"}
        or not isinstance(size, list)
        or len(size) != 2
        or len(dimensions) != 2
        or not isinstance(mean, list)
        or len(mean) != 3
        or not isinstance(std, list)
        or len(std) != 3
        or any(not math.isfinite(value) for value in means + standard_deviations)
        or any(value == 0.0 for value in standard_deviations)
        or any(value <= 0 or value > MAX_MODEL_INPUT_DIMENSION for value in dimensions)
        or math.prod(dimensions) > MAX_MODEL_INPUT_PIXELS
    ):
        raise BackendUnavailable("invalid preprocessing manifest")
    return config


def _letterbox(
    image: Image.Image, config: dict[str, Any]
) -> tuple[np.ndarray, float, float, float]:
    target_height, target_width = (int(value) for value in config["size"])
    if target_height <= 0 or target_width <= 0:
        raise BackendUnavailable("invalid model input size")
    source_width, source_height = image.size
    scale = min(target_width / source_width, target_height / source_height)
    resized_width = max(1, round(source_width * scale))
    resized_height = max(1, round(source_height * scale))
    resized = image.convert("RGB").resize(
        (resized_width, resized_height), Image.Resampling.BILINEAR
    )
    canvas = Image.new("RGB", (target_width, target_height))
    pad_x = float((target_width - resized_width) // 2)
    pad_y = float((target_height - resized_height) // 2)
    canvas.paste(resized, (int(pad_x), int(pad_y)))
    array = np.asarray(canvas, dtype=np.float32) / np.float32(255.0)
    if config["color"] == "BGR":
        array = array[..., ::-1]
    mean = np.asarray(config["mean"], dtype=np.float32)
    std = np.asarray(config["std"], dtype=np.float32)
    array = (array - mean) / std
    if config["layout"] == "NCHW":
        array = np.transpose(array, (2, 0, 1))
    return np.expand_dims(np.ascontiguousarray(array), 0), scale, pad_x, pad_y


def _iou(left: np.ndarray, right: np.ndarray) -> float:
    x1 = max(float(left[0]), float(right[0]))
    y1 = max(float(left[1]), float(right[1]))
    x2 = min(float(left[2]), float(right[2]))
    y2 = min(float(left[3]), float(right[3]))
    intersection = max(0.0, x2 - x1) * max(0.0, y2 - y1)
    left_area = max(0.0, float(left[2] - left[0])) * max(0.0, float(left[3] - left[1]))
    right_area = max(0.0, float(right[2] - right[0])) * max(0.0, float(right[3] - right[1]))
    union = left_area + right_area - intersection
    return intersection / union if union > 0.0 else 0.0


def _nms(boxes: np.ndarray, scores: np.ndarray, classes: np.ndarray, threshold: float) -> list[int]:
    retained: list[int] = []
    order = sorted(range(len(scores)), key=lambda index: (-float(scores[index]), index))
    for index in order:
        if all(
            int(classes[index]) != int(classes[other])
            or _iou(boxes[index], boxes[other]) <= threshold
            for other in retained
        ):
            retained.append(index)
            if len(retained) >= MAX_ANALYSIS_INSTANCES:
                break
    return retained


class OnnxBackend:
    """ONNX backend whose preprocessing and outputs are defined by the bundle manifest."""

    name: Literal["onnx"] = "onnx"

    def __init__(self, bundle: Bundle, runtime: Any) -> None:
        self.bundle = bundle
        self.model_bundle = bundle.info
        self.thresholds = bundle.thresholds
        self._runtime = runtime
        self._models = _as_mapping(bundle.manifest.get("models"))
        self._detector = _as_mapping(self._models.get("detector"))
        self._encoder = _as_mapping(self._models.get("crop_encoder"))
        self._detector_input = _input_config(self._detector)
        self._encoder_input = _input_config(self._encoder)
        outputs = self._detector.get("outputs")
        classes = self._detector.get("classes")
        nms = _as_mapping(self._detector.get("nms"))
        if (
            not isinstance(outputs, list)
            or len(outputs) != 3
            or not all(isinstance(output, str) and output for output in outputs)
            or classes != ["person", "head", "face"]
            or not isinstance(self._encoder.get("output"), str)
            or not isinstance(self._encoder.get("dim"), int)
            or isinstance(self._encoder.get("dim"), bool)
            or not 0 < self._encoder["dim"] <= MAX_EMBEDDING_DIMENSION
            or self._encoder.get("normalize") not in {"l2", "none"}
        ):
            raise BackendUnavailable("invalid model manifest")
        try:
            nms_iou = float(nms["iou"])
            nms_conf = float(nms["conf"])
        except (KeyError, TypeError, ValueError) as exc:
            raise BackendUnavailable("invalid NMS manifest") from exc
        if not 0.0 <= nms_iou <= 1.0 or not 0.0 <= nms_conf <= 1.0:
            raise BackendUnavailable("invalid NMS manifest")
        providers = bundle.manifest.get("providers")
        if (
            not isinstance(providers, list)
            or len(providers) > 16
            or any(
                not isinstance(provider, str) or not 0 < len(provider) <= 256
                for provider in providers
            )
            or "CPUExecutionProvider" not in providers
        ):
            raise BackendUnavailable("CPU execution provider is required")
        available = set(runtime.get_available_providers())
        requested = tuple(str(provider) for provider in providers if str(provider) in available)
        if "CPUExecutionProvider" not in requested:
            raise BackendUnavailable("CPU execution provider is unavailable")
        self.providers = requested
        self._detector_session: Any | None = None
        self._encoder_session: Any | None = None
        self._load_lock = threading.Lock()

    def _sessions(self) -> tuple[Any, Any]:
        if self._detector_session is not None and self._encoder_session is not None:
            return self._detector_session, self._encoder_session
        with self._load_lock:
            if self._detector_session is None:
                detector_bytes = self.bundle.verified_bytes(str(self._detector["file"]))
                encoder_bytes = self.bundle.verified_bytes(str(self._encoder["file"]))
                try:
                    self._detector_session = self._runtime.InferenceSession(
                        detector_bytes, providers=list(self.providers)
                    )
                    self._encoder_session = self._runtime.InferenceSession(
                        encoder_bytes, providers=list(self.providers)
                    )
                except Exception as exc:
                    self._detector_session = None
                    self._encoder_session = None
                    raise BackendUnavailable("ONNX model loading failed") from exc
                self.bundle.release_verified_bytes()
        return self._detector_session, self._encoder_session

    def _encode(self, crop: Image.Image, session: Any) -> bytes:
        tensor, _, _, _ = _letterbox(crop, self._encoder_input)
        output_name = self._encoder.get("output")
        if not isinstance(output_name, str):
            raise BackendUnavailable("invalid encoder output declaration")
        try:
            output = session.run([output_name], {session.get_inputs()[0].name: tensor})[0]
        except Exception as exc:
            raise BackendUnavailable("ONNX encoder inference failed") from exc
        values = np.asarray(output, dtype=np.float32).reshape(-1)
        dimension = int(self._encoder.get("dim", 0))
        if values.size != dimension or dimension <= 0:
            raise BackendUnavailable("encoder output dimension mismatch")
        normalize = self._encoder.get("normalize")
        if normalize == "l2":
            norm = float(np.linalg.norm(values))
            if not math.isfinite(norm) or norm == 0.0:
                raise BackendUnavailable("invalid encoder output")
            values = values / np.float32(norm)
        elif normalize != "none":
            raise BackendUnavailable("invalid encoder normalization")
        return np.asarray(values, dtype="<f4").tobytes()

    def analyze(self, image_bytes: bytes, *, tagger: bool = False) -> Analysis:
        del tagger
        try:
            with Image.open(io.BytesIO(image_bytes)) as opened:
                image = opened.convert("RGB")
                image.load()
        except (OSError, ValueError) as exc:
            raise ValueError("request body is not a supported image") from exc
        detector_session, encoder_session = self._sessions()
        tensor, scale, pad_x, pad_y = _letterbox(image, self._detector_input)
        outputs = self._detector.get("outputs")
        classes = self._detector.get("classes")
        nms_config = _as_mapping(self._detector.get("nms"))
        if not isinstance(outputs, list) or len(outputs) != 3 or not isinstance(classes, list):
            raise BackendUnavailable("invalid detector output declaration")
        try:
            raw_boxes, raw_scores, raw_classes = detector_session.run(
                [str(value) for value in outputs],
                {detector_session.get_inputs()[0].name: tensor},
            )
        except Exception as exc:
            raise BackendUnavailable("ONNX detector inference failed") from exc
        boxes = np.asarray(raw_boxes, dtype=np.float32).reshape(-1, 4)
        scores_array = np.asarray(raw_scores, dtype=np.float32)
        classes_array = np.asarray(raw_classes).reshape(-1)
        if scores_array.ndim > 1 and scores_array.shape[-1] > 1:
            classes_array = np.argmax(scores_array, axis=-1).reshape(-1)
            scores_array = np.max(scores_array, axis=-1)
        scores = scores_array.reshape(-1)
        if len(boxes) != len(scores) or len(scores) != len(classes_array):
            raise BackendUnavailable("inconsistent detector outputs")
        confidence_threshold = float(nms_config.get("conf"))
        eligible = np.flatnonzero(scores >= confidence_threshold)
        if len(eligible) > MAX_DETECTOR_CANDIDATES:
            ranked = sorted(eligible, key=lambda index: (-float(scores[index]), int(index)))
            eligible = np.asarray(ranked[:MAX_DETECTOR_CANDIDATES], dtype=np.intp)
        boxes = boxes[eligible]
        scores = scores[eligible]
        classes_array = classes_array[eligible]
        retained = _nms(boxes, scores, classes_array, float(nms_config.get("iou")))

        width, height = image.size
        instances: list[Instance] = []
        for index in retained:
            class_index = int(classes_array[index])
            if not 0 <= class_index < len(classes) or classes[class_index] not in {
                "person",
                "head",
                "face",
            }:
                raise BackendUnavailable("unsupported detector class")
            raw_x1 = (float(boxes[index, 0]) - pad_x) / scale
            raw_y1 = (float(boxes[index, 1]) - pad_y) / scale
            raw_x2 = (float(boxes[index, 2]) - pad_x) / scale
            raw_y2 = (float(boxes[index, 3]) - pad_y) / scale
            raw_area = max(0.0, raw_x2 - raw_x1) * max(0.0, raw_y2 - raw_y1)
            x1 = min(float(width), max(0.0, raw_x1))
            y1 = min(float(height), max(0.0, raw_y1))
            x2 = min(float(width), max(0.0, raw_x2))
            y2 = min(float(height), max(0.0, raw_y2))
            if x2 <= x1 or y2 <= y1:
                continue
            clipped_area = (x2 - x1) * (y2 - y1)
            visibility = clipped_area / raw_area if raw_area > 0.0 else 0.0
            confidence = float(scores[index])
            passed, flags = _quality(
                crop_width=x2 - x1,
                crop_height=y2 - y1,
                confidence=confidence,
                visibility=visibility,
                thresholds=self.thresholds,
            )
            crop = image.crop((math.floor(x1), math.floor(y1), math.ceil(x2), math.ceil(y2)))
            embedding = self._encode(crop, encoder_session)
            instances.append(
                Instance(
                    kind=cast(Literal["person", "head", "face"], classes[class_index]),
                    bbox=(x1 / width, y1 / height, (x2 - x1) / width, (y2 - y1) / height),
                    det_conf=confidence,
                    quality_passed=passed,
                    quality_flags=flags,
                    embedding=embedding,
                    embedding_dim=int(self._encoder["dim"]),
                )
            )
        return Analysis(model_version=self.bundle.info.version, instances=tuple(instances))


def select_backend(
    model_dir: Path | None = None,
    trusted_digests: set[str] | frozenset[str] | None = None,
) -> Backend:
    """Select ONNX only after integrity and runtime checks; otherwise use mock."""
    bundle = discover_bundle(model_dir, trusted_digests)
    if bundle is None:
        return MockBackend()
    try:
        runtime = importlib.import_module("onnxruntime")
        return OnnxBackend(bundle, runtime)
    except (BackendUnavailable, ImportError, OSError, TypeError, ValueError):
        return MockBackend(bundle.thresholds)


class BackendManager:
    """Owns a replaceable backend without retaining request data or results."""

    def __init__(self, backend: Backend | None = None, model_dir: Path | None = None) -> None:
        self._backend: Backend = backend or select_backend(model_dir)
        self._lock = threading.Lock()

    @property
    def backend(self) -> Backend:
        return self._backend

    def analyze(self, image_bytes: bytes, *, tagger: bool = False) -> Analysis:
        try:
            return self._backend.analyze(image_bytes, tagger=tagger)
        except BackendUnavailable:
            with self._lock:
                thresholds = self._backend.thresholds
                self._backend = MockBackend(thresholds)
            return self._backend.analyze(image_bytes, tagger=tagger)
