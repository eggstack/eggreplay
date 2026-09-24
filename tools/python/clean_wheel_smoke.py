"""Install one wheel into a disposable environment and smoke it off-checkout."""

from __future__ import annotations

import argparse
import glob
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import venv


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--wheel", type=Path, required=True)
    parser.add_argument("--smoke-script", type=Path, required=True)
    args = parser.parse_args()
    wheel = args.wheel
    if wheel.is_dir():
        matches = sorted(wheel.glob("*.whl"))
        assert len(matches) == 1, matches
        wheel = matches[0]
    elif "*" in str(wheel):
        matches = [Path(item) for item in sorted(glob.glob(str(wheel)))]
        assert len(matches) == 1, matches
        wheel = matches[0]
    wheel = wheel.resolve()
    smoke_script = args.smoke_script.resolve()
    assert wheel.is_file()

    with tempfile.TemporaryDirectory(prefix="eggreplay-clean-install-") as temporary:
        root = Path(temporary)
        environment = root / "venv"
        smoke_cwd = root / "outside-checkout"
        smoke_cwd.mkdir()
        venv.EnvBuilder(with_pip=True).create(environment)
        python = environment / ("Scripts/python.exe" if os.name == "nt" else "bin/python")
        subprocess.run(
            [str(python), "-m", "pip", "install", f"{wheel}[dev]"],
            check=True,
        )
        clean_env = os.environ.copy()
        clean_env.pop("PYTHONPATH", None)
        clean_env.pop("PYTHONHOME", None)
        subprocess.run(
            [str(python), str(smoke_script)],
            check=True,
            cwd=smoke_cwd,
            env=clean_env,
        )


if __name__ == "__main__":
    main()
