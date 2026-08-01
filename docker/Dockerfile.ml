# syntax=docker/dockerfile:1

# 本番では監督側で digest を確定後、Dockerfile.server と同じ @sha256 形式で固定する。
# digest 確定前のため、ここでは意図的にタグ指定のみにしている。
FROM python:3.12-slim AS runtime

COPY --from=ghcr.io/astral-sh/uv:0.8.15 /uv /uvx /bin/

RUN groupadd --gid 1000 illumia \
    && useradd --uid 1000 --gid 1000 --create-home --shell /usr/sbin/nologin illumia \
    && mkdir -p /app/ml /data/models /run/illumia \
    && chown -R illumia:illumia /app /data /run/illumia \
    && chmod 0755 /app /app/ml \
    && chmod 0700 /data /run/illumia

WORKDIR /app/ml

COPY ml/pyproject.toml ml/uv.lock ml/README.md ./
COPY ml/illumia_ml/ ./illumia_ml/

# pyproject.toml の runtime 依存 (onnxruntime を含む) を lockfile どおりに導入する。
RUN --mount=type=cache,id=illumia-uv-cache,target=/root/.cache/uv,sharing=locked \
    uv sync --frozen --no-dev

ENV ILLUMIA_MODEL_DIR=/data/models \
    PATH="/app/ml/.venv/bin:${PATH}"

# /run/illumia は Compose の共有 named volume で置き換える前提。
VOLUME ["/run/illumia"]

USER 1000:1000

HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
    CMD ["python", "-c", "import http.client as h,socket,sys;c=h.HTTPConnection('localhost');c.sock=socket.socket(socket.AF_UNIX);c.sock.connect('/run/illumia/ml.sock');c.request('GET','/ml/v1/health');r=c.getresponse();sys.exit(0 if r.status == 200 else 1)"]

STOPSIGNAL SIGTERM
ENTRYPOINT ["illumia-ml", "--socket", "/run/illumia/ml.sock"]
