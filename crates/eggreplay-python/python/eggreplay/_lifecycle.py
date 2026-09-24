"""Small Python orchestration helpers over the native Rust lifecycle."""

from __future__ import annotations

import asyncio
from pathlib import Path
from typing import TYPE_CHECKING, Any, Callable, TypeVar

if TYPE_CHECKING:
    from . import RecordMode
    from ._native import Server

T = TypeVar("T")


async def _close_server(server: Server) -> None:
    await server.aclose()


async def open_server(
    fixture_path: str | Path,
    *,
    record_mode: RecordMode | str | None = None,
    upstream: str | None = None,
    route: str = "direct",
    bind: str = "127.0.0.1:0",
    websockets: bool = False,
) -> Server:
    """Open the shared Rust replay/recording lifecycle for one fixture path."""
    from . import (
        Fixture,
        RecordMode,
        record_policy,
        recording_gateway,
        replay_server,
    )

    path = Path(fixture_path)
    if record_mode is None:
        if websockets:
            raise ValueError("WebSocket acquisition requires an explicit network record mode")
        if not path.is_dir():
            raise FileNotFoundError(f"EggReplay fixture does not exist: {path}")
        return await replay_server(Fixture(str(path)), bind=bind)

    mode = record_mode.value if isinstance(record_mode, RecordMode) else record_mode
    policy = record_policy(
        RecordMode(mode),
        fixture_exists=path.exists(),
        upstream_configured=upstream is not None,
    )
    if policy["mode"] is RecordMode.SEALED:
        if websockets:
            raise ValueError("WebSocket acquisition is unavailable for sealed replay")
        if not path.is_dir():
            raise FileNotFoundError(f"EggReplay fixture does not exist: {path}")
        return await replay_server(Fixture(str(path)), bind=bind)
    if upstream is None:
        raise ValueError("an explicit upstream is required for recording modes")
    return await recording_gateway(
        str(path),
        upstream,
        bind=bind,
        route=route,
        record_mode=policy["mode"].value,
        websockets=websockets,
    )


class _FixtureContext:
    def __init__(self, fixture_path: str | Path, **options: Any) -> None:
        self.fixture_path = fixture_path
        self.options = options
        self.server: Server | None = None
        self._runner: asyncio.Runner | None = None

    def __enter__(self) -> Server:
        if self.server is not None:
            raise RuntimeError("EggReplay fixture context is already open")
        self._runner = asyncio.Runner()
        try:
            self.server = self._runner.run(open_server(self.fixture_path, **self.options))
        except BaseException:
            self._runner.close()
            self._runner = None
            raise
        return self.server

    def __exit__(self, exc_type: Any, exc: Any, traceback: Any) -> bool:
        if self.server is not None and self._runner is not None:
            try:
                self._runner.run(_close_server(self.server))
            finally:
                self.server = None
                self._runner.close()
                self._runner = None
        return False

    async def __aenter__(self) -> Server:
        if self.server is not None:
            raise RuntimeError("EggReplay fixture context is already open")
        self.server = await open_server(self.fixture_path, **self.options)
        return self.server

    async def __aexit__(self, exc_type: Any, exc: Any, traceback: Any) -> bool:
        if self.server is not None:
            try:
                await self.server.aclose()
            finally:
                self.server = None
        return False


def fixture_context(
    fixture_path: str | Path,
    *,
    record_mode: RecordMode | str | None = None,
    upstream: str | None = None,
    route: str = "direct",
    bind: str = "127.0.0.1:0",
    websockets: bool = False,
) -> _FixtureContext:
    """VCR-style context syntax backed by the same Rust server authority."""
    return _FixtureContext(
        fixture_path,
        record_mode=record_mode,
        upstream=upstream,
        route=route,
        bind=bind,
        websockets=websockets,
    )


def use_fixture(
    fixture_path: str | Path,
    *,
    record_mode: RecordMode | str | None = None,
    upstream: str | None = None,
    route: str = "direct",
    websockets: bool = False,
) -> Callable[[Callable[..., T]], Callable[..., T]]:
    """Mark a pytest function to use the managed EggReplay server fixture."""
    def decorate(function: Callable[..., T]) -> Callable[..., T]:
        try:
            import pytest
        except ImportError as error:  # pragma: no cover - pytest is optional
            raise RuntimeError("use_fixture requires pytest") from error
        if inspect.iscoroutinefunction(function):
            marker = pytest.mark.eggreplay_async_fixture
        else:
            marker = pytest.mark.eggreplay_fixture
        marked = marker(
            str(fixture_path),
            record_mode=record_mode,
            upstream=upstream,
            route=route,
            websockets=websockets,
        )(function)
        return pytest.mark.usefixtures(
            "eggreplay_async_server" if inspect.iscoroutinefunction(function) else "eggreplay_server"
        )(marked)

    import inspect

    return decorate
