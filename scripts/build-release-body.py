#!/usr/bin/env python3
"""Combine bilingual release-note files into one GitHub Release body."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--tag", required=True)
    parser.add_argument("--notes-dir", default="release-notes")
    parser.add_argument("--output", required=True)
    args = parser.parse_args()
    directory = Path(args.notes_dir) / args.tag
    zh_path = directory / "zh-CN.md"
    en_path = directory / "en.md"
    if not zh_path.is_file() or not en_path.is_file():
        print(f"release notes are incomplete for {args.tag}", file=sys.stderr)
        return 1
    zh = zh_path.read_text(encoding="utf-8").strip()
    en = en_path.read_text(encoding="utf-8").strip()
    if not zh or not en:
        print(f"release notes must not be empty for {args.tag}", file=sys.stderr)
        return 1
    body = (
        "<details open>\n"
        "<summary>中文</summary>\n\n"
        f"{zh}\n\n"
        "</details>\n\n"
        "<details>\n"
        "<summary>English</summary>\n\n"
        f"{en}\n\n"
        "</details>\n"
    )
    output = Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(body, encoding="utf-8")
    print(f"Wrote {output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
