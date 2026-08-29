# illumia-ml

ステートレスな ML サイドカー。仕様は
[docs/07_ml_integration.md](../docs/07_ml_integration.md) と
[docs/13_model_requirements.md](../docs/13_model_requirements.md) を参照。

```bash
uv sync
uv run illumia-ml --socket /path/to/illumia-ml.sock
uv run pytest
uv run ruff check .
```

TCP listen は提供しない。`ILLUMIA_MODEL_DIR` 配下から checksum 検証済みかつ
`ILLUMIA_TRUSTED_MODEL_DIGESTS` で外部 pin された最新モデルバンドルを選択する。
pin が無い・不一致・利用不能の場合は決定的な mock バックエンドで全 API を提供する。
検証済み ONNX bytes を runtime へ直接渡すため、hash 検証後に model path を reopen しない。
