FROM python:3.12-slim@sha256:57cd7c3a7a273101a6485ba99423ee568157882804b1124b4dd04266317710de AS runtime

COPY --from=ghcr.io/astral-sh/uv:0.8.15@sha256:a5727064a0de127bdb7c9d3c1383f3a9ac307d9f2d8a391edc7896c54289ced0 /uv /uvx /bin/

RUN apt-get update \
    && apt-get upgrade -y --no-install-recommends \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 2000 illumia-socket \
    && groupadd --gid 10001 illumia \
    && useradd --uid 10001 --gid 10001 --groups 2000 --create-home --shell /usr/sbin/nologin illumia \
    && mkdir -p /app/ml /models /run/illumia \
    && chown -R illumia:illumia /app /models /run/illumia \
    && chmod 0755 /app /app/ml \
    && chmod 0755 /models \
    && chmod 0770 /run/illumia

WORKDIR /app/ml

COPY ml/pyproject.toml ml/uv.lock ml/README.md ./
COPY ml/illumia_ml/ ./illumia_ml/

# pyproject.toml の runtime 依存 (onnxruntime を含む) を lockfile どおりに導入する。
RUN --mount=type=cache,id=illumia-uv-cache,target=/root/.cache/uv,sharing=locked \
    uv sync --frozen --no-dev

ENV ILLUMIA_MODEL_DIR=/models \
    PATH="/app/ml/.venv/bin:${PATH}"

# /run/illumia は Compose の共有 named volume で置き換える前提。
VOLUME ["/run/illumia"]

USER 10001:10001

HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
    CMD ["uv", "run", "--frozen", "--no-sync", "python", "-c", "import http.client as h,socket,sys;c=h.HTTPConnection('localhost');c.sock=socket.socket(socket.AF_UNIX);c.sock.connect('/run/illumia/ml.sock');c.request('GET','/ml/v1/health');r=c.getresponse();sys.exit(0 if r.status == 200 else 1)"]

STOPSIGNAL SIGTERM
ENTRYPOINT ["illumia-ml", "--socket", "/run/illumia/ml.sock"]
