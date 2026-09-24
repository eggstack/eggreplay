"""Write checksums for one set of distribution artifacts."""

from __future__ import annotations

import argparse
import hashlib
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("directory", type=Path)
    parser.add_argument("--pattern", default="*")
    args = parser.parse_args()
    files = sorted(path for path in args.directory.glob(args.pattern) if path.is_file())
    assert files, f"no artifacts matched {args.directory / args.pattern}"
    lines = [
        f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}"
        for path in files
    ]
    (args.directory / "SHA256SUMS.txt").write_text("\n".join(lines) + "\n", encoding="utf-8")
    print("\n".join(lines))


if __name__ == "__main__":
    main()
