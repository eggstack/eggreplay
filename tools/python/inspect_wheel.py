"""Validate wheel contents and metadata before smoke installation."""

from __future__ import annotations

import argparse
import hashlib
from email.parser import Parser
from pathlib import Path
import zipfile


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("wheel", type=Path)
    parser.add_argument("platform_tag")
    args = parser.parse_args()
    wheel = args.wheel.resolve()
    assert wheel.is_file() and wheel.suffix == ".whl"
    assert f"cp311-abi3-{args.platform_tag}.whl" in wheel.name, wheel.name

    with zipfile.ZipFile(wheel) as archive:
        names = archive.namelist()
        assert "eggreplay/py.typed" in names
        assert "eggreplay/__init__.pyi" in names
        assert "eggreplay/pytest_plugin.pyi" in names
        assert "eggreplay/pytest_plugin.py" in names
        assert any(name.endswith(".dist-info/METADATA") for name in names)
        metadata_name = next(name for name in names if name.endswith(".dist-info/METADATA"))
        metadata = Parser().parsestr(archive.read(metadata_name).decode("utf-8"))
        assert metadata["Name"] == "eggreplay"
        assert metadata["Version"] == "0.1.0"
        assert metadata["Requires-Python"] == ">=3.11"
        assert metadata["License-Expression"] == "MIT"
        assert "https://github.com/eggstack/eggreplay" in metadata.as_string()

        forbidden_parts = {".git", ".venv", "target", "tests", "fixtures", "__pycache__"}
        bad = [
            name
            for name in names
            if forbidden_parts.intersection(Path(name).parts)
            or name.endswith((".pyc", ".pyo"))
        ]
        assert not bad, bad
        native = [
            name
            for name in names
            if name.endswith((".so", ".pyd", ".dll", ".dylib"))
        ]
        assert len(native) == 1 and native[0].startswith("eggreplay/_native."), native

    digest = hashlib.sha256(wheel.read_bytes()).hexdigest()
    print(f"{digest}  {wheel.name}")


if __name__ == "__main__":
    main()
