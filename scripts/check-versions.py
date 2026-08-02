#!/usr/bin/env python3
"""全コンポーネントのバージョンが一致していることを検証する (docs/12: SEC-006)。

タグ `vX.Y.Z` で release すると、GHCR のタグは `vX.Y.Z` でも server API や
package metadata が別バージョンを返す、という不整合を防ぐ。

使い方:
    uv run scripts/check-versions.py            # 相互一致だけを検証
    uv run scripts/check-versions.py --tag v0.2.0  # タグとの一致も検証
    uv run scripts/check-versions.py --set 0.2.0   # 全 manifest を書き換える

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

    def regex(self) -> re.Pattern[str]:
        return re.compile(self.pattern, re.MULTILINE)


SITES: tuple[VersionSite, ...] = (
    VersionSite("Cargo.toml", r'^version\s*=\s*"([^"]+)"'),
    VersionSite("web/package.json", r'^\s*"version":\s*"([^"]+)"'),
    VersionSite("ml/pyproject.toml", r'^version\s*=\s*"([^"]+)"'),
    VersionSite("apps/android/package.json", r'^\s*"version":\s*"([^"]+)"'),
    VersionSite("apps/android/src-tauri/Cargo.toml", r'^version\s*=\s*"([^"]+)"'),
    VersionSite("apps/android/src-tauri/tauri.conf.json", r'^\s*"version":\s*"([^"]+)"'),
)

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
    parser.add_argument("--tag", help="検証するタグ (例 v0.2.0)")
    parser.add_argument("--set", dest="new_version", help="全 manifest をこのバージョンに揃える")
    args = parser.parse_args()

    if args.new_version:
        if not SEMVER.match(args.new_version):
            raise SystemExit(f"バージョン形式が不正: {args.new_version}")
        for site in SITES:
            changed = write_version(site, args.new_version)
            print(f"{'updated' if changed else 'unchanged'}: {site.path}")
        return 0

    versions = {site.path: read_version(site) for site in SITES}
    for path, version in versions.items():
        print(f"{path}: {version}")

    unique = set(versions.values())
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
