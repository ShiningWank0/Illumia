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

TCP listen は提供しない。`ILLUMIA_MODEL_DIR` 配下からチェックサム検証済みの最新モデル
バンドルを選択し、利用できない場合は決定的な mock バックエンドで全 API を提供する。
