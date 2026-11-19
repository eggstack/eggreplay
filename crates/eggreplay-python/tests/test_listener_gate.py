"""M021D: the Python replay/recording listeners are loopback by default.

`replay_server`, `recording_gateway`, and the high-level `fixture_context`
helper all bind an **unauthenticated** listener. Binding beyond loopback is an
explicit opt-in and every other address is a `ConfigurationError`; TLS is not
authentication, and this surface has no TLS listener at all.
"""

import asyncio
import inspect
import shutil
from pathlib import Path

import pytest

import eggreplay

STORE_FIXTURES = (
    Path(__file__).resolve().parents[2] / "eggreplay-store" / "tests" / "fixtures"
)


def empty_fixture(tmp_path: Path, name: str = "fixture.eggr") -> Path:
    """Copy a known-good empty fixture so each test owns its own copy."""
    destination = tmp_path / name
    shutil.copytree(STORE_FIXTURES / "schema-1-empty", destination)
    return destination


def test_replay_server_accepts_loopback_by_default(tmp_path):
    fixture = eggreplay.Fixture(str(empty_fixture(tmp_path)))

    async def run():
        server = await eggreplay.replay_server(fixture)
        assert server.address.startswith("127.0.0.1:")
        await server.aclose()

    asyncio.run(run())


@pytest.mark.parametrize("bind", ["0.0.0.0:0", "[::]:0"])
def test_replay_server_refuses_wildcard_binds_by_default(tmp_path, bind):
    fixture = eggreplay.Fixture(str(empty_fixture(tmp_path)))

    async def run():
        with pytest.raises(eggreplay.ConfigurationError) as caught:
            await eggreplay.replay_server(fixture, bind=bind)
        message = str(caught.value)
        assert "unauthenticated" in message, message
        assert "TLS does not add authentication" in message, message
        assert "allow_non_loopback" in message, message
        # Value-free: the refusal never names the fixture it would have served.
        assert str(tmp_path) not in message, message

    asyncio.run(run())


def test_replay_server_allows_non_loopback_with_the_keyword(tmp_path):
    fixture = eggreplay.Fixture(str(empty_fixture(tmp_path)))

    async def run():
        server = await eggreplay.replay_server(
            fixture, bind="0.0.0.0:0", allow_non_loopback=True
        )
        assert server.address.startswith("0.0.0.0:")
        await server.aclose()

    asyncio.run(run())


@pytest.mark.parametrize("bind", ["0.0.0.0:0", "[::]:0"])
def test_recording_gateway_refuses_wildcard_binds_by_default(tmp_path, bind):
    destination = tmp_path / "recorded.eggr"

    async def run():
        with pytest.raises(eggreplay.ConfigurationError) as caught:
            await eggreplay.recording_gateway(
                str(destination), "http://127.0.0.1:9", bind=bind
            )
        message = str(caught.value)
        assert "unauthenticated" in message, message
        assert "allow_non_loopback" in message, message
        assert not destination.exists(), "a refused bind must not create the fixture"

    asyncio.run(run())


def test_recording_gateway_allows_non_loopback_with_the_keyword(tmp_path):
    destination = tmp_path / "recorded.eggr"

    async def run():
        server = await eggreplay.recording_gateway(
            str(destination),
            "http://127.0.0.1:9",
            bind="0.0.0.0:0",
            allow_non_loopback=True,
        )
        assert server.address.startswith("0.0.0.0:")
        await server.aclose()
        assert destination.is_dir(), "an empty recording still publishes"

    asyncio.run(run())


def test_fixture_context_forwards_the_explicit_opt_in(tmp_path):
    fixture = empty_fixture(tmp_path)

    async def run():
        async with eggreplay.fixture_context(
            fixture, bind="0.0.0.0:0", allow_non_loopback=True
        ) as server:
            assert server.address.startswith("0.0.0.0:")

    asyncio.run(run())


def test_fixture_context_still_refuses_without_the_opt_in(tmp_path):
    fixture = empty_fixture(tmp_path)

    async def run():
        with pytest.raises(eggreplay.ConfigurationError):
            async with eggreplay.fixture_context(fixture, bind="0.0.0.0:0"):
                pass

    asyncio.run(run())


def test_invalid_bind_keeps_the_parse_error(tmp_path):
    fixture = eggreplay.Fixture(str(empty_fixture(tmp_path)))

    async def run():
        for allow in (False, True):
            with pytest.raises(eggreplay.ConfigurationError) as caught:
                await eggreplay.replay_server(
                    fixture, bind="not-an-address", allow_non_loopback=allow
                )
            assert "socket address" in str(caught.value), str(caught.value)
        with pytest.raises(eggreplay.ConfigurationError) as caught:
            await eggreplay.recording_gateway(
                str(tmp_path / "unused.eggr"),
                "http://127.0.0.1:9",
                bind="not-an-address",
                allow_non_loopback=True,
            )
        assert "socket address" in str(caught.value), str(caught.value)

    asyncio.run(run())


def test_the_opt_in_is_the_last_keyword_and_bind_stays_positional(tmp_path):
    fixture = eggreplay.Fixture(str(empty_fixture(tmp_path)))

    for function in (eggreplay.replay_server, eggreplay.recording_gateway):
        parameters = list(inspect.signature(function).parameters)
        assert parameters[-1] == "allow_non_loopback", parameters

    async def run():
        # `bind` was the second positional parameter before M021D and still is.
        server = await eggreplay.replay_server(fixture, "127.0.0.1:0")
        assert server.address.startswith("127.0.0.1:")
        await server.aclose()

    asyncio.run(run())
