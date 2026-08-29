#!/usr/bin/env python3
"""全コンポーネントのバージョンが一致していることを検証する (docs/12: SEC-006)。

タグ `vX.Y.Z` で release すると、GHCR のタグは `vX.Y.Z` でも server API や
package metadata が別バージョンを返す、という不整合を防ぐ。

使い方:
    uv run scripts/check-versions.py              # 相互一致だけを検証
    uv run scripts/check-versions.py --tag v1.0.0 # タグとの一致も検証
    uv run scripts/check-versions.py --set 1.0.0  # 正本metadataを書き換える

Python は必ず uv 経由で実行する (AGENTS.md 6)。
"""

from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent


@dataclass(frozen=True)
class VersionSite:
    """バージョンが書かれている 1 箇所。"""

    path: str
    #: version 文字列を group(1) で捕まえる正規表現。
    pattern: str
    #: 同じlockfile内の複数componentを区別する表示名。
    label: str | None = None

    def regex(self) -> re.Pattern[str]:
        return re.compile(self.pattern, re.MULTILINE)

    @property
    def display_name(self) -> str:
        return self.label or self.path


SOURCE_SITES: tuple[VersionSite, ...] = (
    VersionSite("Cargo.toml", r'^version\s*=\s*"([^"]+)"'),
    VersionSite("web/package.json", r'^\s*"version":\s*"([^"]+)"'),
    VersionSite("ml/pyproject.toml", r'^version\s*=\s*"([^"]+)"'),
    VersionSite("ml/illumia_ml/__init__.py", r'^__version__\s*=\s*"([^"]+)"'),
    VersionSite("apps/android/package.json", r'^\s*"version":\s*"([^"]+)"'),
    VersionSite("apps/android/src-tauri/Cargo.toml", r'^version\s*=\s*"([^"]+)"'),
    VersionSite("apps/android/src-tauri/tauri.conf.json", r'^\s*"version":\s*"([^"]+)"'),
)

DERIVED_SITES: tuple[VersionSite, ...] = (
    VersionSite(
        "Cargo.lock",
        r'^\[\[package\]\]\nname = "illumia-core"\nversion = "([^"]+)"',
        "Cargo.lock:illumia-core",
    ),
    VersionSite(
        "Cargo.lock",
        r'^\[\[package\]\]\nname = "illumia-desktop"\nversion = "([^"]+)"',
        "Cargo.lock:illumia-desktop",
    ),
    VersionSite(
        "Cargo.lock",
        r'^\[\[package\]\]\nname = "illumia-server"\nversion = "([^"]+)"',
        "Cargo.lock:illumia-server",
    ),
    VersionSite(
        "web/package-lock.json",
        r'^\{\n  "name": "illumia-web",\n  "version": "([^"]+)"',
        "web/package-lock.json:root",
    ),
    VersionSite(
        "web/package-lock.json",
        r'^    "": \{\n      "name": "illumia-web",\n      "version": "([^"]+)"',
        "web/package-lock.json:packages-root",
    ),
    VersionSite(
        "ml/uv.lock",
        r'^\[\[package\]\]\nname = "illumia-ml"\nversion = "([^"]+)"',
        "ml/uv.lock:illumia-ml",
    ),
    VersionSite(
        "apps/android/package-lock.json",
        r'^\{\n  "name": "illumia-android",\n  "version": "([^"]+)"',
        "apps/android/package-lock.json:root",
    ),
    VersionSite(
        "apps/android/package-lock.json",
        r'^    "": \{\n      "name": "illumia-android",\n      "version": "([^"]+)"',
        "apps/android/package-lock.json:packages-root",
    ),
    VersionSite(
        "apps/android/src-tauri/Cargo.lock",
        r'^\[\[package\]\]\nname = "illumia-android"\nversion = "([^"]+)"',
        "apps/android/src-tauri/Cargo.lock:illumia-android",
    ),
)

ALL_SITES: tuple[VersionSite, ...] = SOURCE_SITES + DERIVED_SITES

SEMVER = re.compile(r"^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$")


def read_version(site: VersionSite) -> str:
    path = REPO_ROOT / site.path
    if not path.exists():
        raise SystemExit(f"{site.path}: ファイルが見つからない")
    match = site.regex().search(path.read_text(encoding="utf-8"))
    if match is None:
        raise SystemExit(f"{site.path}: version を特定できない")
    return match.group(1)


def write_version(site: VersionSite, version: str) -> bool:
    """version を書き換える。変更したら True。"""
    path = REPO_ROOT / site.path
    text = path.read_text(encoding="utf-8")
    match = site.regex().search(text)
    if match is None:
        raise SystemExit(f"{site.path}: version を特定できない")
    if match.group(1) == version:
        return False
    start, end = match.span(1)
    path.write_text(text[:start] + version + text[end:], encoding="utf-8")
    return True


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--tag", help="検証するタグ (例 v1.0.0)")
    parser.add_argument(
        "--set", dest="new_version", help="正本metadataをこのバージョンに揃える"
    )
    args = parser.parse_args()

    if args.new_version:
        if not SEMVER.match(args.new_version):
            raise SystemExit(f"バージョン形式が不正: {args.new_version}")
        for site in SOURCE_SITES:
            changed = write_version(site, args.new_version)
            print(f"{'updated' if changed else 'unchanged'}: {site.path}")
        print("lockfileは各package managerで再生成してから通常検証を実行すること")
        return 0

    versions = [(site.display_name, read_version(site)) for site in ALL_SITES]
    for name, version in versions:
        print(f"{name}: {version}")

    unique = {version for _name, version in versions}
    if len(unique) != 1:
        print("\nERROR: コンポーネント間でバージョンが一致していない", file=sys.stderr)
        return 1
    current = unique.pop()

    if args.tag is not None:
        expected = args.tag[1:] if args.tag.startswith("v") else args.tag
        if expected != current:
            print(
                f"\nERROR: タグ {args.tag} と実装のバージョン {current} が一致しない",
                file=sys.stderr,
            )
            return 1
        print(f"\nOK: タグ {args.tag} と全コンポーネントが {current} で一致")
    else:
        print(f"\nOK: 全コンポーネントが {current} で一致")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
