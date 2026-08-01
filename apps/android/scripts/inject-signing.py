#!/usr/bin/env python3
"""tauri android init が生成する app/build.gradle.kts に release 署名設定を注入する。

gen/android は CI で生成され (非コミット)、その内容はバージョン依存で変わりうる。
このスクリプトは冪等で、署名設定が未注入のときだけ挿入する。keystore.properties
(password / keyAlias / storeFile) は同ディレクトリ (gen/android) に置く前提。

Tauri 公式ドキュメント "Sign the APK" の Kotlin DSL 手順に準拠。ローカルでは
Android SDK/NDK が無く実ビルド検証できないため、CI 実行時にのみ効果を検証する。
"""

import sys
from pathlib import Path

SIGNING_BLOCK = """
    signingConfigs {
        create("release") {
            val props = java.util.Properties()
            val propFile = rootProject.file("keystore.properties")
            if (propFile.exists()) {
                props.load(java.io.FileInputStream(propFile))
                keyAlias = props["keyAlias"] as String
                keyPassword = props["password"] as String
                storeFile = file(props["storeFile"] as String)
                storePassword = props["password"] as String
            }
        }
    }
"""


def inject(path: Path) -> None:
    text = path.read_text(encoding="utf-8")
    if "signingConfigs {" in text and 'create("release")' in text:
        print(f"[inject-signing] already configured: {path}")
        return

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
            + "\n            signingConfig = signingConfigs.getByName(\"release\")"
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
    print(f"[inject-signing] release signing injected into {path}")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: inject-signing.py <app/build.gradle.kts>")
    inject(Path(sys.argv[1]))
