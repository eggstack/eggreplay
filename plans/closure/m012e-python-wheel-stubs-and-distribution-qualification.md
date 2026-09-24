# M012E — Wheels, Typing, and Distribution Qualification Closure

Status: closed

## Implementation

- Qualifying revision: `95e3686e0cd29086528b21bc991372e446154eb8`.
- Distribution/import name is `eggreplay`; metadata requires Python `>=3.11`,
  declares MIT licensing and repository URLs, includes `py.typed` and public
  stubs, registers the pytest plugin, and has no runtime Python dependencies.
- The ABI remains `abi3-py311`, using the M012A qualification. Public stubs
  cover the runtime exports, Rust-backed enum member names, data and report
  wrappers, sync/async lifecycle APIs, and pytest helpers. The API-manifest
  test checks those names against runtime exports.
- The clean wheel smoke installs into a new virtual environment outside the
  checkout and checks package origin, fixture loading, replay server startup,
  regression, and pytest plugin use. The workflow also inspects tags, package
  contents, metadata, and extension count, and emits SHA256 manifests.
- The first hosted attempt exposed PowerShell's literal handling of wildcard
  arguments. The wheel checks now take a directory and resolve its single
  wheel themselves. The corrected full run passed on every required platform.

## Hosted evidence

- Full wheel and sdist qualification passed:
  [GitHub Actions run 35967068775](https://github.com/eggstack/eggreplay/actions/runs/35967068775).
- Standard CI passed on the same revision:
  [GitHub Actions run 35967055070](https://github.com/eggstack/eggreplay/actions/runs/35967055070).
- The wheel run built on native runners and passed clean-install smokes for
  CPython 3.11 on Linux x86_64, Linux aarch64, macOS arm64, macOS x86_64, and
  Windows x86_64. Linux aarch64 was runtime-smoked on the native
  `ubuntu-24.04-arm` runner.
- The same `manylinux_2_34_x86_64` wheel passed the full fixture/replay/
  regression/pytest smoke under CPython 3.11, 3.12, 3.13, and 3.14 in four
  separate hosted jobs. The standard CI run also retains the Python binding
  test matrix and same-wheel CPython 3.11-to-3.14 check.
- Local checks before hosted qualification: Python suite, 30 passed on the
  available compatible environment; `uv lock --check`, wheel inspection,
  clean wheel smoke, sdist inspection, and `git diff --check` passed. A local
  macOS x86_64 wheel clean smoke also passed.

## Qualified artifacts

Each uploaded artifact contains the named wheel and its `SHA256SUMS.txt`
manifest. The hashes below are SHA256 digests of the complete uploaded
artifact archives, as reported by GitHub Actions; they protect the wheel and
its embedded wheel hash manifest as a unit.

| Wheel filename | Tag | Uploaded artifact | Artifact SHA256 |
|---|---|---|
| `eggreplay-0.1.0-cp311-abi3-manylinux_2_34_x86_64.whl` | `cp311-abi3-manylinux_2_34_x86_64` | `eggreplay-manylinux-x86_64` | `7c8caaf7562c6cec08098b95e982e878e67e1b1b36dd51106de646361ba63889` |
| `eggreplay-0.1.0-cp311-abi3-manylinux_2_34_aarch64.whl` | `cp311-abi3-manylinux_2_34_aarch64` | `eggreplay-manylinux-aarch64` | `bba7b1f8ac8dedc8f572f0b1c8c8b06b9dc6d767025b51575fc626ed782360f8` |
| `eggreplay-0.1.0-cp311-abi3-macosx_11_0_arm64.whl` | `cp311-abi3-macosx_11_0_arm64` | `eggreplay-macos-arm64` | `3a3e4e3cd0bc79bc4dc9043cbc1580abf98df40cd21d5fc83b2289d505bcbafc` |
| `eggreplay-0.1.0-cp311-abi3-macosx_10_12_x86_64.whl` | `cp311-abi3-macosx_10_12_x86_64` | `eggreplay-macos-x86_64` | `4ef6c7f11f2a7df7b2425b50ae6e6a086f6ed9d8dc6cf546e5f574d1aff56c0e` |
| `eggreplay-0.1.0-cp311-abi3-win_amd64.whl` | `cp311-abi3-win_amd64` | `eggreplay-windows-x86_64` | `373fcfa4e69ffbfc903ba0f609dee382cf17d2fcdae583e7291fa6a3ee6453ca` |
| `eggreplay-0.1.0.tar.gz` | source distribution | `eggreplay-sdist` | `ce2a6f431cff3e44067ef385ffa66b5ba8fbe221a2136349b489a8b3b93d8c3c` |

The sdist inspector confirms the archive includes the workspace sources,
Cargo lockfile, Python package, stubs, typing marker, and build metadata, with
no build output, local virtual environment, fixture directory, or bytecode.
The sdist job builds a wheel from the archive. Maturin prunes unused workspace
members when creating the sdist; for that job only, `--locked` is omitted
because Cargo otherwise asks to remove lockfile entries for the pruned CLI
member. The committed lockfile is included in the sdist and the wheel build
from it succeeds.

## Support and handoff

Qualified: CPython 3.11–3.14 with the GIL on Linux x86_64, Linux aarch64,
macOS arm64, macOS x86_64, and Windows x86_64. The wheels use the `abi3-py311`
stable ABI and were built as `cp311-abi3` artifacts.

Unsupported/unqualified: musllinux, Windows arm64, PyPy, GraalPy, CPython 3.15,
free-threaded CPython, and other architectures. No package was published to
PyPI.

M012E is closed. M012F is ready; M013 and later plans remain blocked on their
declared dependencies.
