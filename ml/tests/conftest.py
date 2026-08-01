from __future__ import annotations

import io

import pytest
from fastapi.testclient import TestClient
from PIL import Image

from illumia_ml.app import create_app
from illumia_ml.backends import BackendManager, MockBackend


@pytest.fixture
def image_bytes() -> bytes:
    output = io.BytesIO()
    Image.new("RGB", (160, 120), color=(37, 88, 151)).save(output, format="PNG")
    return output.getvalue()


@pytest.fixture
def client() -> TestClient:
    app = create_app(manager=BackendManager(backend=MockBackend()))
    with TestClient(app) as test_client:
        yield test_client
