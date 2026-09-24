"""Exercise a wheel from an isolated environment without importing the checkout."""

from __future__ import annotations

import asyncio
import hashlib
import json
import os
from pathlib import Path
import tempfile
import zipfile

import eggreplay
import pytest


def write_fixture(path: Path) -> None:
    payload = b"wheel-smoke"
    digest = hashlib.sha256(payload).hexdigest()
    (path / "blobs").mkdir(parents=True)
    (path / "blobs" / digest).write_bytes(payload)
    body = {"kind": "blob", "sha256": digest, "length": len(payload)}
    manifest = {
        "schema_version": 1,
        "tool_version": eggreplay.version(),
        "session_id": "wheel-smoke",
        "capture_mode": "semantic",
        "source": None,
        "target": None,
        "matcher_profile": "strict",
        "redaction_profile": "default-v1",
        "complete": True,
        "flow_count": 1,
        "blob_count": 1,
    }
    flow = {
        "schema_version": 1,
        "id": "wheel-flow",
        "started_at_ms": 1,
        "completed_at_ms": 2,
        "request": {
            "method": "POST",
            "scheme": "http",
            "authority": "example.test",
            "path": "/items",
            "query": [],
            "headers": [
                {"name": "content-length", "value": str(len(payload))},
                {"name": "connection", "value": "close"},
            ],
            "body": body,
            "trailers": [],
        },
        "outcome": {
            "kind": "response",
            "status": 200,
            "headers": [
                {"name": "content-length", "value": str(len(payload))},
                {"name": "connection", "value": "close"},
            ],
            "body": body,
            "trailers": [],
        },
        "physical_route": None,
        "provenance": {"mode": "wheel-smoke", "observer": "wheel-smoke"},
        "annotations": [],
        "redactions": [],
    }
    (path / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
    (path / "flows.jsonl").write_text(json.dumps(flow) + "\n", encoding="utf-8")


async def raw_request(address: str) -> bytes:
    host, port = address.rsplit(":", 1)
    reader, writer = await asyncio.open_connection(host, int(port))
    writer.write(
        b"POST /items HTTP/1.1\r\n"
        b"Host: example.test\r\n"
        b"Content-Length: 11\r\n"
        b"Connection: close\r\n\r\n"
        b"wheel-smoke"
    )
    await writer.drain()
    response = await reader.read()
    writer.close()
    await writer.wait_closed()
    return response


async def smoke_native_api(fixture_path: Path) -> None:
    fixture = eggreplay.Fixture(str(fixture_path))
    assert fixture.flow("wheel-flow").request.method == "POST"
    with fixture.open_body("wheel-flow", "request") as body:
        assert body.read_all(max_bytes=32) == b"wheel-smoke"

    server = await eggreplay.replay_server(fixture)
    try:
        response = await raw_request(server.address)
        assert b"200" in response and response.endswith(b"wheel-smoke")
        report = await eggreplay.regress_flow(
            fixture,
            "wheel-flow",
            f"http://{server.address}",
        )
        assert report.baseline_flow_ids == ["wheel-flow"]
    finally:
        await server.aclose()


def smoke_pytest_plugin(fixture_path: Path, temp_dir: Path) -> None:
    test_file = temp_dir / "test_installed_wheel_plugin.py"
    test_file.write_text(
        "def test_plugin_fixture(eggreplay_fixture, eggreplay_server):\n"
        "    assert eggreplay_fixture.flow('wheel-flow').id == 'wheel-flow'\n"
        "    assert eggreplay_server.address.startswith('127.0.0.1:')\n",
        encoding="utf-8",
    )
    result = pytest.main(
        [
            "-q",
            "--eggreplay-fixture",
            str(fixture_path),
            str(test_file),
        ],
        plugins=[],
    )
    assert result == pytest.ExitCode.OK


def main() -> None:
    installed_package = Path(eggreplay.__file__).resolve()
    assert "eggreplay-python/python" not in str(installed_package).replace(os.sep, "/")
    with tempfile.TemporaryDirectory(prefix="eggreplay-wheel-smoke-") as temporary:
        temp_dir = Path(temporary)
        fixture_path = temp_dir / "smoke.eggr"
        fixture_path.mkdir()
        write_fixture(fixture_path)
        asyncio.run(smoke_native_api(fixture_path))
        smoke_pytest_plugin(fixture_path, temp_dir)


if __name__ == "__main__":
    main()
