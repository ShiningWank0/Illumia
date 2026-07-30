# illumia-ml

ML サイドカー (ステートレス)。仕様は [docs/07_ml_integration.md](../docs/07_ml_integration.md)。
実装は M4。それまでは uv プロジェクトの scaffold のみ。

```bash
uv sync          # 依存取得 (Python 3.12 も自動取得)
uv run pytest    # テスト
uv run ruff check .
```
