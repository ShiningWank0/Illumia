#!/usr/bin/env python3
"""tauri android init が生成する app/build.gradle.kts に release 署名設定を注入する。

gen/android は CI で生成され (非コミット)、その内容はバージョン依存で変わりうる。
このスクリプトは冪等で、署名設定が未注入のときだけ挿入する。keystore.properties
(password / keyAlias / storeFile) は同ディレクトリ (gen/android) に置く前提。

Tauri 公式ドキュメント "Sign the APK" の Kotlin DSL 手順に準拠。ローカルでは
Android SDK/NDK が無く実ビルド検証できないため、CI 実行時にのみ効果を検証する。

**Kotlin DSL の注意**: `android { ... }` の内側では `java` が Gradle の
JavaPluginExtension に解決され、`java.util.Properties` / `java.io.FileInputStream`
のような完全修飾名が "Unresolved reference: util" で失敗する。そのため
ファイル先頭で明示的に import し、短い名前で参照する。
"""

import sys
from pathlib import Path

# `android { }` の内側で `java.*` が使えないため先頭で import する。
IMPORTS = ("java.io.FileInputStream", "java.util.Properties")

SIGNING_BLOCK = """
    signingConfigs {
        create("release") {
            val propFile = rootProject.file("keystore.properties")
            if (propFile.exists()) {
                val props = Properties()
                FileInputStream(propFile).use { stream -> props.load(stream) }
                // getProperty は String を返すのでキャスト不要
                // (`as String` は "No cast needed" でコンパイルエラーになる)。
                keyAlias = props.getProperty("keyAlias")
                keyPassword = props.getProperty("password")
                storeFile = file(props.getProperty("storeFile"))
                storePassword = props.getProperty("password")
            }
        }
    }
"""


def add_imports(text: str) -> str:
    """必要な import をファイル先頭へ (冪等に) 追加する。"""
    missing = [name for name in IMPORTS if f"import {name}" not in text]
    if not missing:
        return text
    header = "".join(f"import {name}\n" for name in missing)
    return header + text


def inject(path: Path) -> None:
    text = path.read_text(encoding="utf-8")
    if "signingConfigs {" in text and 'create("release")' in text:
        print(f"[inject-signing] already configured: {path}")
        return

    text = add_imports(text)

    idx = text.find("android {")
    if idx == -1:
        raise SystemExit(f"[inject-signing] `android {{` block not found in {path}")
    brace = text.find("{", idx)
    text = text[: brace + 1] + SIGNING_BLOCK + text[brace + 1 :]

    # release ビルドタイプに署名設定を紐付ける。
    needle = 'getByName("release") {'
    pos = text.find(needle)
    if pos != -1:
        insert_at = pos + len(needle)
        text = (
            text[:insert_at]
            + '\n            signingConfig = signingConfigs.getByName("release")'
            + text[insert_at:]
        )
    else:
        # release ブロックが無ければ buildTypes ごと追加する。
        anchor = text.find("android {")
        b = text.find("{", anchor)
        text = (
            text[: b + 1]
            + '\n    buildTypes {\n        getByName("release") {\n'
            + '            signingConfig = signingConfigs.getByName("release")\n'
            + "        }\n    }\n"
            + text[b + 1 :]
        )

    path.write_text(text, encoding="utf-8")

    # 注入後の状態を検証する。壊れた gradle を無言で残さない。
    written = path.read_text(encoding="utf-8")
    for name in IMPORTS:
        if f"import {name}" not in written:
            raise SystemExit(f"[inject-signing] import が入っていない: {name}")
    if "signingConfig = signingConfigs.getByName(\"release\")" not in written:
        raise SystemExit("[inject-signing] release ビルドタイプへの紐付けに失敗した")
    # import 行そのものは完全修飾名を含むので、本文だけを見る。
    body = "\n".join(
        line for line in written.splitlines() if not line.startswith("import ")
    )
    for name in IMPORTS:
        if name in body:
            raise SystemExit(
                f"[inject-signing] `{name}` の完全修飾参照が本文に残っている "
                "(android ブロック内では解決できない)"
            )
    print(f"[inject-signing] release signing injected into {path}")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: inject-signing.py <app/build.gradle.kts>")
    inject(Path(sys.argv[1]))
