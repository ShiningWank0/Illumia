"""Unix-domain-socket-only command line entry point."""

from __future__ import annotations

import argparse
from collections.abc import Sequence

import uvicorn


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="illumia-ml")
    parser.add_argument("--socket", required=True, help="Unix domain socket path")
    return parser


def main(argv: Sequence[str] | None = None) -> None:
    args = build_parser().parse_args(argv)
    uvicorn.run(
        "illumia_ml.app:create_app",
        factory=True,
        uds=args.socket,
        access_log=False,
    )
