"""Pytest fixtures for explicit, sealed-by-default EggReplay tests."""

from __future__ import annotations

import asyncio
import json
import os
from pathlib import Path
from typing import Any, AsyncIterator, Iterator, TYPE_CHECKING

import pytest

from . import ConfigurationError, FixtureError, RecordMode, RegressionReport, regress_flow
from ._lifecycle import _close_server, open_server

if TYPE_CHECKING:
    from ._native import Fixture, Server


def pytest_addoption(parser: pytest.Parser) -> None:
    group = parser.getgroup("eggreplay", "EggReplay fixture replay and explicit recording")
    group.addoption(
        "--eggreplay-fixture",
        action="store",
        default=None,
        metavar="PATH",
        help="explicit .eggr fixture path (relative paths resolve from pytest root)",
    )
    group.addoption(
        "--eggreplay-record-mode",
        choices=("once", "append-new", "re-record"),
        default=None,
        help="explicitly enable fixture creation/update; omitted means sealed replay",
    )
    group.addoption(
        "--eggreplay-upstream",
        action="store",
        default=None,
        metavar="URL",
        help="required upstream for an explicit network recording mode",
    )
    group.addoption(
        "--eggreplay-route",
        action="store",
        default="direct",
        metavar="ROUTE",
        help="direct or an Eggress outbound route expression",
    )
    group.addoption(
        "--eggreplay-bind",
        action="store",
        default="127.0.0.1:0",
        metavar="ADDRESS",
        help="local EggServe address for the managed server fixture",
    )
    group.addoption(
        "--eggreplay-websockets",
        action="store_true",
        default=False,
        help="explicitly enable WebSocket acquisition in a recording mode",
    )


def pytest_configure(config: pytest.Config) -> None:
    config.addinivalue_line(
        "markers",
        "eggreplay_fixture(path, record_mode=None, upstream=None, route='direct'): explicit fixture for a sync test",
    )
    config.addinivalue_line(
        "markers",
        "eggreplay_async_fixture(path, record_mode=None, upstream=None, route='direct'): explicit fixture for an async test",
    )


def _settings(
    request: pytest.FixtureRequest,
) -> tuple[Path, str | None, str | None, str, str, bool]:
    marker = request.node.get_closest_marker("eggreplay_fixture") or request.node.get_closest_marker(
        "eggreplay_async_fixture"
    )
    marker_path = None
    marker_mode = marker_upstream = marker_route = marker_websockets = None
    if marker is not None:
        marker_path = marker.kwargs.get("path") or (marker.args[0] if marker.args else None)
        marker_mode = marker.kwargs.get("record_mode")
        marker_upstream = marker.kwargs.get("upstream")
        marker_route = marker.kwargs.get("route")
        marker_websockets = marker.kwargs.get("websockets")
    raw_path = marker_path or request.config.getoption("eggreplay_fixture")
    if not raw_path:
        pytest.fail(
            "EggReplay requires an explicit fixture path: pass --eggreplay-fixture=PATH "
            "or use @eggreplay.use_fixture(PATH)",
            pytrace=False,
        )
    path = Path(raw_path)
    if not path.is_absolute():
        root = Path(request.config.rootpath).resolve()
        path = (root / path).resolve()
        if not path.is_relative_to(root):
            pytest.fail(
                "relative EggReplay fixture paths must stay inside the pytest root",
                pytrace=False,
            )
    else:
        path = path.resolve()
    mode = marker_mode if marker_mode is not None else request.config.getoption("eggreplay_record_mode")
    upstream = marker_upstream if marker_upstream is not None else request.config.getoption("eggreplay_upstream")
    route = marker_route if marker_route is not None else request.config.getoption("eggreplay_route")
    bind = request.config.getoption("eggreplay_bind")
    websockets = (
        marker_websockets
        if marker_websockets is not None
        else request.config.getoption("eggreplay_websockets")
    )
    return path, mode, upstream, route, bind, websockets


class _WriterLock:
    """Atomic sibling lock. A stale lock always needs explicit recovery."""

    def __init__(self, fixture_path: Path, worker_id: str) -> None:
        self.path = fixture_path.with_name(f".{fixture_path.name}.eggreplay.lock")
        self.worker_id = worker_id
        self.owned = False

    def __enter__(self) -> _WriterLock:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        metadata = json.dumps(
            {"pid": os.getpid(), "worker_id": self.worker_id},
            separators=(",", ":"),
        ).encode()
        try:
            descriptor = os.open(
                self.path,
                os.O_CREAT | os.O_EXCL | os.O_WRONLY,
                0o600,
            )
        except FileExistsError as error:
            try:
                prior = self.path.read_text(encoding="utf-8")[:512]
            except OSError:
                prior = "unreadable lock metadata"
            raise RuntimeError(
                f"EggReplay fixture already has a writer lock ({prior}); "
                "verify the recorded PID and worker are no longer active, then remove "
                f"the stale lock explicitly: {self.path}"
            ) from error
        try:
            remaining = memoryview(metadata)
            while remaining:
                written = os.write(descriptor, remaining)
                if written == 0:
                    raise OSError("could not write EggReplay fixture lock metadata")
                remaining = remaining[written:]
            os.close(descriptor)
        except BaseException:
            try:
                os.close(descriptor)
            except OSError:
                pass
            self.path.unlink(missing_ok=True)
            raise
        self.owned = True
        return self

    def __exit__(self, exc_type: Any, exc: Any, traceback: Any) -> bool:
        if self.owned:
            self.path.unlink(missing_ok=True)
            self.owned = False
        return False


def _writer_lock(
    request: pytest.FixtureRequest,
    path: Path,
    mode: str | None,
    upstream: str | None,
) -> _WriterLock | None:
    if mode is None:
        return None
    try:
        from . import record_policy

        policy = record_policy(
            RecordMode(mode),
            fixture_exists=path.exists(),
            upstream_configured=upstream is not None,
        )
    except (ValueError, ConfigurationError, FixtureError) as error:
        pytest.fail(f"invalid EggReplay record policy: {error}", pytrace=False)
    effective, upstream_enabled = policy["mode"].value, policy["upstream_enabled"]
    if upstream_enabled and upstream is None:
        pytest.fail("an explicit upstream is required for this record mode", pytrace=False)
    if effective == "sealed":
        return None
    worker_id = getattr(request.config, "workerinput", {}).get("workerid", "master")
    return _WriterLock(path, worker_id)


def _open_options(request: pytest.FixtureRequest) -> tuple[Path, dict[str, Any], _WriterLock | None]:
    path, mode, upstream, route, bind, websockets = _settings(request)
    lock = _writer_lock(request, path, mode, upstream)
    options: dict[str, Any] = {"bind": bind}
    if mode is not None:
        options.update(record_mode=mode, upstream=upstream, route=route, websockets=websockets)
    elif websockets:
        pytest.fail("--eggreplay-websockets requires an explicit record mode", pytrace=False)
    return path, options, lock


@pytest.fixture
def eggreplay_fixture(request: pytest.FixtureRequest) -> Fixture:
    """Open the selected fixture as a validated, read-only Rust session."""
    path, _mode, _upstream, _route, _bind, _websockets = _settings(request)
    if not path.is_dir():
        pytest.fail(f"EggReplay fixture does not exist: {path}", pytrace=False)
    from . import Fixture

    try:
        return Fixture(str(path))
    except FixtureError as error:
        pytest.fail(f"EggReplay fixture is invalid: {error}", pytrace=False)


@pytest.fixture
def eggreplay_server(request: pytest.FixtureRequest) -> Iterator[Server]:
    """Managed synchronous replay/record server; teardown always closes it."""
    path, options, lock = _open_options(request)
    if lock is not None:
        try:
            lock.__enter__()
        except RuntimeError as error:
            pytest.fail(str(error), pytrace=False)
    with asyncio.Runner() as runner:
        server = None
        try:
            server = runner.run(open_server(path, **options))
            yield server
        finally:
            try:
                if server is not None:
                    runner.run(_close_server(server))
            finally:
                if lock is not None:
                    lock.__exit__(None, None, None)


try:
    import pytest_asyncio
except ImportError:  # pragma: no cover - pytest-asyncio is an optional test dependency
    pytest_asyncio = None


if pytest_asyncio is not None:

    @pytest_asyncio.fixture
    async def eggreplay_async_server(request: pytest.FixtureRequest) -> AsyncIterator[Server]:
        """Managed async server fixture for pytest-asyncio tests."""
        path, options, lock = _open_options(request)
        if lock is not None:
            try:
                lock.__enter__()
            except RuntimeError as error:
                pytest.fail(str(error), pytrace=False)
        server = None
        try:
            server = await open_server(path, **options)
            yield server
        finally:
            try:
                if server is not None:
                    await server.aclose()
            finally:
                if lock is not None:
                    lock.__exit__(None, None, None)

else:

    @pytest.fixture
    def eggreplay_async_server():
        pytest.fail("eggreplay_async_server requires pytest-asyncio", pytrace=False)


@pytest.fixture
def eggreplay_recorder(eggreplay_server: Server) -> Server:
    """Alias for the explicitly configured managed recording server."""
    return eggreplay_server


if pytest_asyncio is not None:

    @pytest_asyncio.fixture
    async def eggreplay_async_recorder(eggreplay_async_server: Server) -> Server:
        """Async alias for an explicitly configured recording server."""
        return eggreplay_async_server


class RegressionAssertions:
    def __init__(self, fixture: Any) -> None:
        self.fixture = fixture

    @staticmethod
    def _message(report: RegressionReport) -> str:
        details = [f"{finding.kind} {finding.field}" for finding in report.findings[:20]]
        suffix = "\n... more findings omitted" if report.finding_count > 20 else ""
        heading = f"EggReplay regression failed ({report.finding_count} finding(s)):"
        return heading + ("\n" + "\n".join(details) if details else "") + suffix

    @classmethod
    def _assert_success(cls, report: RegressionReport) -> RegressionReport:
        if not report.success:
            error = AssertionError(cls._message(report))
            error.report = report
            raise error
        return report

    def compare(self, flow_id: str, target: str, **options: Any) -> RegressionReport:
        async def compare_async() -> RegressionReport:
            return await regress_flow(self.fixture, flow_id, target, **options)

        with asyncio.Runner() as runner:
            report = runner.run(compare_async())
        return self._assert_success(report)

    async def acompare(self, flow_id: str, target: str, **options: Any) -> RegressionReport:
        report = await regress_flow(self.fixture, flow_id, target, **options)
        return self._assert_success(report)


@pytest.fixture
def eggreplay_report(eggreplay_fixture: Fixture) -> RegressionAssertions:
    """Bounded assertion/report helper over Rust candidate regression."""
    return RegressionAssertions(eggreplay_fixture)
