"""Inspect the source archive produced by maturin."""

from __future__ import annotations

import argparse
import hashlib
from pathlib import Path
import tarfile


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("archive", type=Path)
    args = parser.parse_args()
    archive = args.archive.resolve()
    assert archive.is_file() and archive.name.endswith(".tar.gz")
    with tarfile.open(archive, "r:gz") as bundle:
        names = bundle.getnames()
        members = {Path(name).as_posix() for name in names}
        roots = {Path(name).parts[0] for name in names if Path(name).parts}
        assert len(roots) == 1, roots
        root = next(iter(roots))
        required = {
            f"{root}/PKG-INFO",
            f"{root}/Cargo.lock",
            f"{root}/Cargo.toml",
            f"{root}/LICENSE-MIT",
            f"{root}/README.md",
            f"{root}/crates/eggreplay-core/Cargo.toml",
            f"{root}/crates/eggreplay-store/Cargo.toml",
            f"{root}/crates/eggreplay-http/Cargo.toml",
            f"{root}/crates/eggreplay-python/Cargo.toml",
            f"{root}/crates/eggreplay-python/src/lib.rs",
            f"{root}/pyproject.toml",
            f"{root}/python/eggreplay/__init__.py",
            f"{root}/python/eggreplay/__init__.pyi",
            f"{root}/python/eggreplay/pytest_plugin.py",
            f"{root}/python/eggreplay/pytest_plugin.pyi",
            f"{root}/python/eggreplay/py.typed",
        }
        assert required <= members, sorted(required - members)
        forbidden_parts = {".git", ".venv", "target", "__pycache__", "fixtures"}
        bad = [
            name
            for name in names
            if forbidden_parts.intersection(Path(name).parts)
            or name.endswith((".pyc", ".pyo"))
        ]
        assert not bad, bad
        metadata = bundle.extractfile(f"{root}/PKG-INFO")
        assert metadata is not None
        text = metadata.read().decode("utf-8")
        assert "Name: eggreplay" in text
        assert "Version: 0.1.0" in text
        assert "Requires-Python: >=3.11" in text

    print(f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}")


if __name__ == "__main__":
    main()
