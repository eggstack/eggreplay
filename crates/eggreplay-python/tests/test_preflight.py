import asyncio
import ast
import gc
import hashlib
import json
from pathlib import Path
import pytest

import eggreplay
from eggreplay import _native

pytest_plugins = ["pytester"]


def test_native_import_and_roundtrip():
    assert _native.version() == "0.1.0"


def test_stub_manifest_matches_runtime_exports_and_rust_enum_names():
    stub = Path(eggreplay.__file__).with_suffix(".pyi")
    assert stub.is_file()
    assert Path(eggreplay.__file__).with_name("py.typed").is_file()
    tree = ast.parse(stub.read_text())
    declarations = {
        node.name
        for node in tree.body
        if isinstance(node, (ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef))
    }
    assert set(eggreplay.__all__) <= declarations

    for enum_name in (
        "MatcherProfile",
        "ConsumptionMode",
        "RecordMode",
        "WebSocketRedaction",
    ):
        runtime_enum = getattr(eggreplay, enum_name)
        stub_enum = next(
            node
            for node in tree.body
            if isinstance(node, ast.ClassDef) and node.name == enum_name
        )
        stub_members = {
            node.target.id
            for node in stub_enum.body
            if isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name)
        }
        assert stub_members == set(runtime_enum.__members__)

    plugin_stub = Path(eggreplay.__file__).with_name("pytest_plugin.pyi")
    plugin_tree = ast.parse(plugin_stub.read_text())
    plugin_declarations = {
        node.name
        for node in plugin_tree.body
        if isinstance(node, (ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef))
    }
    assert {
        "RegressionAssertions",
        "eggreplay_fixture",
        "eggreplay_server",
        "eggreplay_async_server",
        "eggreplay_recorder",
        "eggreplay_async_recorder",
        "eggreplay_report",
    } <= plugin_declarations


def test_async_runtime_bridge():
    async def read_value():
        return await eggreplay.async_value()

    assert asyncio.run(read_value()) == 42


def test_async_cancellation():
    async def cancel_sleep():
        task = asyncio.ensure_future(eggreplay.async_sleep(30))
        await asyncio.sleep(0.01)
        task.cancel()
        try:
            await task
        except asyncio.CancelledError:
            return
        raise AssertionError("cancellation must reach the Rust future")

    asyncio.run(cancel_sleep())


def write_fixture(
    root: Path,
    payload: bytes = b"body",
    with_extensions: bool = False,
    with_error: bool = False,
) -> tuple[Path, str]:
    root.mkdir()
    digest = hashlib.sha256(payload).hexdigest()
    (root / "blobs").mkdir()
    (root / "blobs" / digest).write_bytes(payload)
    manifest = {
        "schema_version": 1,
        "tool_version": "0.1.0",
        "session_id": "python-test",
        "capture_mode": "semantic",
        "source": None,
        "target": None,
        "matcher_profile": "strict",
        "redaction_profile": "default-v1",
        "complete": True,
        "flow_count": 2 if with_error else 1,
        "blob_count": 1,
    }
    extension_docs = {
        "websocket-messages": ("websockets.jsonl", {"schema_version": 1, "conversations": []}),
        "rules": ("rules.json", {"schema_version": 1, "scenarios": []}),
        "stream-events": ("stream-events.json", {"schema_version": 1, "flows": []}),
    }
    if with_extensions:
        manifest["schema_version"] = 2
        manifest["extensions"] = []
        for name, (relative_path, document) in extension_docs.items():
            (root / relative_path).write_text(json.dumps(document))
            manifest["extensions"].append(
                {"name": name, "schema_version": 1, "path": relative_path, "required_for_replay": True}
            )
    body = {"kind": "blob", "sha256": digest, "length": len(payload)}
    flow = {
        "schema_version": 1,
        "id": "flow-1",
        "started_at_ms": 1,
        "completed_at_ms": 2,
        "request": {
            "method": "POST",
            "scheme": "http",
            "authority": "example.test",
            "path": "/items",
            "query": [{"key": "tag", "value": "a"}, {"key": "tag", "value": "b"}],
            "headers": [
                {"name": "x-repeat", "value": "one"},
                {"name": "x-repeat", "value": "two"},
            ],
            "body": body,
            "trailers": [],
        },
        "outcome": {
            "kind": "response",
            "status": 200,
            "headers": [],
            "body": body,
            "trailers": [],
        },
        "physical_route": None,
        "provenance": {"mode": "test", "observer": "python-test"},
        "annotations": [],
        "redactions": [],
    }
    flows = [flow]
    if with_error:
        failed = json.loads(json.dumps(flow))
        failed["id"] = "flow-error"
        failed["request"]["body"] = {"kind": "absent"}
        failed["outcome"] = {
            "kind": "error",
            "category": "timeout",
            "phase": "body",
            "message": "upstream response timed out",
        }
        flows.append(failed)
    (root / "manifest.json").write_text(json.dumps(manifest))
    (root / "flows.jsonl").write_text("".join(json.dumps(item) + "\n" for item in flows))
    return root, digest


def test_fixture_preserves_ordered_duplicates_and_child_lifetime(tmp_path):
    root, _ = write_fixture(tmp_path / "ordered.eggr")
    fixture = eggreplay.Fixture(str(root))
    assert fixture.manifest["flow_count"] == 1
    flows = list(fixture.iter_flows())
    flow = flows[0]
    assert flow.id == "flow-1"
    assert flow.request.method == "POST"
    assert flow.request.query == [("tag", "a"), ("tag", "b")]
    assert flow.request.headers == [("x-repeat", "one"), ("x-repeat", "two")]
    assert flow.response.status == 200
    assert fixture.flow("flow-1").to_dict() == flow.to_dict()
    assert fixture.flow("absent") is None
    data = flow.to_dict()
    assert "flow-1" not in repr(flow)
    assert [(item["key"], item["value"]) for item in data["request"]["query"]] == [
        ("tag", "a"),
        ("tag", "b"),
    ]
    assert [item["value"] for item in data["request"]["headers"]] == ["one", "two"]
    del fixture
    assert flow.id == "flow-1"


def test_flow_error_keeps_rust_category_phase_and_provenance(tmp_path):
    root, _ = write_fixture(tmp_path / "errors.eggr", with_error=True)
    flow = eggreplay.Fixture(str(root)).flow("flow-error")
    assert flow.error.category == "timeout"
    assert flow.error.phase == "body"
    assert flow.error.message == "upstream response timed out"


def test_body_reader_is_bounded_chunked_and_closes(tmp_path):
    root, _ = write_fixture(tmp_path / "body.eggr", b"abcdef")
    fixture = eggreplay.Fixture(str(root))
    reader = fixture.open_body("flow-1", "request")
    assert reader.length == 6
    assert reader.read(2) == b"ab"
    assert reader.read(4) == b"cdef"
    assert reader.read(1) == b""
    reader.close()
    try:
        reader.read(1)
    except eggreplay.FixtureError:
        pass
    else:
        raise AssertionError("closed body reader must reject reads")

    with fixture.open_body("flow-1", "response") as body:
        assert body.read_all(max_bytes=6) == b"abcdef"
    try:
        with fixture.open_body("flow-1", "request") as body:
            body.read_all(max_bytes=5)
    except ValueError:
        pass
    else:
        raise AssertionError("read_all must honor its byte cap")

    with fixture.open_body("flow-1", "request") as body:
        with pytest.raises(eggreplay.FixtureError):
            body.read(8 * 1024 * 1024 + 1)

    reader = fixture.open_body("flow-1", "request")
    del fixture
    gc.collect()
    assert list(reader) == [b"abcdef"]


def test_body_chunk_iteration_stays_bounded(tmp_path):
    payload = b"x" * (140 * 1024)
    root, _ = write_fixture(tmp_path / "chunked.eggr", payload)
    with eggreplay.Fixture(str(root)).open_body("flow-1", "request") as reader:
        chunks = list(reader)
    assert [len(chunk) for chunk in chunks] == [64 * 1024, 64 * 1024, 12 * 1024]
    assert b"".join(chunks) == payload


def test_corrupt_fixture_errors_are_redacted_and_body_digest_is_checked(tmp_path):
    root, digest = write_fixture(tmp_path / "bad-body.eggr", b"safe")
    fixture = eggreplay.Fixture(str(root))
    (root / "blobs" / digest).write_bytes(b"evil")
    try:
        fixture.open_body("flow-1", "request").read_all(max_bytes=16)
    except eggreplay.FixtureError as error:
        assert "evil" not in str(error)
        assert digest not in str(error)
    else:
        raise AssertionError("changed body must fail digest validation")

    corrupt = tmp_path / "corrupt.eggr"
    corrupt.mkdir()
    (corrupt / "manifest.json").write_text("secret payload is not a manifest")
    try:
        eggreplay.Fixture(str(corrupt))
    except eggreplay.FixtureError as error:
        assert "secret" not in str(error)
    else:
        raise AssertionError("corrupt fixture must fail closed")


def test_schema_and_unknown_required_extension_fail_as_fixture_errors(tmp_path):
    schema_root, _ = write_fixture(tmp_path / "bad-schema.eggr")
    manifest = json.loads((schema_root / "manifest.json").read_text())
    manifest["schema_version"] = 999
    (schema_root / "manifest.json").write_text(json.dumps(manifest))
    with pytest.raises(eggreplay.FixtureError):
        eggreplay.Fixture(str(schema_root))

    extension_root, _ = write_fixture(tmp_path / "unknown-extension.eggr")
    manifest = json.loads((extension_root / "manifest.json").read_text())
    manifest["extensions"] = [
        {"name": "future-required", "schema_version": 1, "path": "future.json", "required_for_replay": True}
    ]
    (extension_root / "manifest.json").write_text(json.dumps(manifest))
    with pytest.raises(eggreplay.FixtureError):
        eggreplay.Fixture(str(extension_root))


def test_body_reader_propagates_symlink_rejection(tmp_path):
    root, digest = write_fixture(tmp_path / "symlink.eggr")
    fixture = eggreplay.Fixture(str(root))
    blob = root / "blobs" / digest
    backup = root / "backup"
    blob.rename(backup)
    target = tmp_path / "target-body"
    target.write_bytes(b"body")
    try:
        blob.symlink_to(target)
    except (OSError, NotImplementedError):
        backup.rename(blob)
        pytest.skip("symlink creation is unavailable")
    try:
        with pytest.raises(eggreplay.FixtureError):
            fixture.open_body("flow-1", "request")
    finally:
        blob.unlink()
        backup.rename(blob)


def test_repeated_fixture_children_survive_python_gc(tmp_path):
    root, _ = write_fixture(tmp_path / "gc.eggr", payload=b"gc")
    for _ in range(32):
        fixture = eggreplay.Fixture(str(root))
        iterator = fixture.iter_flows()
        flow = next(iterator)
        reader = fixture.open_body(flow.id, "request")
        del iterator, fixture
        gc.collect()
        assert flow.request.method == "POST"
        assert reader.read_all(max_bytes=8) == b"gc"


def test_extension_summaries_are_bounded_metadata(tmp_path):
    root, _ = write_fixture(tmp_path / "extensions.eggr", with_extensions=True)
    fixture = eggreplay.Fixture(str(root))
    assert fixture.websocket_summary()["conversation_count"] == 0
    assert fixture.scenario_summary()["scenario_count"] == 0
    assert fixture.stream_summary()["event_count"] == 0
    assert {item["name"] for item in fixture.extension_metadata} == {
        "websocket-messages",
        "rules",
        "stream-events",
    }


def test_rust_backed_configuration_enums_and_validation():
    assert eggreplay.MatcherProfile.STRICT.value == "strict"
    assert eggreplay.ConsumptionMode.REPEAT_LAST.value == "repeat-last"
    assert eggreplay.record_policy(
        eggreplay.RecordMode.ONCE, fixture_exists=False, upstream_configured=True
    ) == {"mode": eggreplay.RecordMode.ONCE, "upstream_enabled": True}
    with pytest.raises(eggreplay.ConfigurationError):
        eggreplay.record_policy(
            eggreplay.RecordMode.APPEND_NEW, fixture_exists=False, upstream_configured=False
        )
    assert eggreplay.StreamTimingMode("scaled:1.5").value == "scaled:1.5"
    with pytest.raises(eggreplay.ConfigurationError):
        eggreplay.StreamTimingMode("scaled:0")
    assert "authorization" in eggreplay.RedactionConfig().to_dict()["headers"]
    with pytest.raises(eggreplay.ConfigurationError):
        eggreplay.ComparisonPolicy(sse_ignored=["unknown"])
    assert eggreplay.RouteSpecification("direct").target is None
    with pytest.raises(eggreplay.ConfigurationError):
        eggreplay.RouteSpecification("direct", "http://proxy")
    assert eggreplay.RouteSpecification("eggress", "socks5://127.0.0.1:1080").kind == "eggress"
    with pytest.raises(eggreplay.ConfigurationError):
        eggreplay.RouteSpecification("eggress", "not a route")
    assert eggreplay.WebSocketOptions(recording_enabled=True).to_dict()["recording_enabled"]
    route = eggreplay.RouteSpecification(
        "eggress", "socks5://alice:do-not-print@127.0.0.1:1080"
    )
    assert "do-not-print" not in (route.target or "")
    assert "do-not-print" not in repr(route)


def test_regression_report_uses_rust_json_shape():
    payload = {
        "schema_version": 2,
        "scheduler": "sequential",
        "baseline_flow_ids": ["flow-1"],
        "findings": [
            {
                "kind": "body",
                "field": "response.body",
                "baseline": "sha256:abc",
                "candidate": "sha256:def",
            }
        ],
    }
    report = eggreplay.RegressionReport.from_json(json.dumps(payload))
    assert not report.success
    assert report.finding_count == 1
    assert report.findings[0].kind == "body"
    assert json.loads(report.to_json()) == report.to_dict()
    assert report.to_dict() == payload
    with pytest.raises(eggreplay.RegressionError):
        eggreplay.RegressionReport.from_json("not a report")


def test_pytest_report_failure_is_bounded_and_retains_structured_report():
    from eggreplay.pytest_plugin import RegressionAssertions

    report = eggreplay.RegressionReport.from_json(
        json.dumps(
            {
                "schema_version": 2,
                "scheduler": "sequential",
                "baseline_flow_ids": ["flow-1"],
                "findings": [
                    {
                        "kind": "header",
                        "field": "authorization",
                        "baseline": "secret-baseline-value",
                        "candidate": "secret-candidate-value",
                    }
                ],
            }
        )
    )
    with pytest.raises(AssertionError) as raised:
        RegressionAssertions._assert_success(report)
    assert "authorization" in str(raised.value)
    assert "secret-baseline-value" not in str(raised.value)
    assert "secret-candidate-value" not in str(raised.value)
    assert raised.value.report.to_dict() == report.to_dict()


async def raw_request(address, body=b"body", path="/items"):
    host, port = address.rsplit(":", 1)
    reader, writer = await asyncio.open_connection(host, int(port))
    request = (
        f"POST {path}?tag=a&tag=b HTTP/1.1\r\n".encode()
        + b"Host: example.test\r\n"
        + b"x-repeat: one\r\n"
        + b"x-repeat: two\r\n"
        + f"Content-Length: {len(body)}\r\nConnection: close\r\n\r\n".encode()
        + body
    )
    writer.write(request)
    await writer.drain()
    response = await reader.read()
    writer.close()
    await writer.wait_closed()
    return response


def test_async_replay_server_lifecycle_and_server_isolation(tmp_path):
    root, _ = write_fixture(tmp_path / "server.eggr")
    flow = json.loads((root / "flows.jsonl").read_text())
    flow["request"]["headers"].extend(
        [{"name": "content-length", "value": "4"}, {"name": "connection", "value": "close"}]
    )
    (root / "flows.jsonl").write_text(json.dumps(flow) + "\n")

    async def run():
        fixture = eggreplay.Fixture(str(root))
        first = await eggreplay.replay_server(fixture)
        second = await eggreplay.replay_server(fixture)
        assert first.address != second.address
        assert b"200" in await raw_request(first.address)
        assert b"200" in await raw_request(second.address)
        await first.aclose()
        await first.aclose()
        assert second.is_closing() is False
        async with second:
            assert second.is_closing() is False
        assert second.is_closing() is True

    asyncio.run(run())


def test_recording_gateway_finalizes_after_async_context(tmp_path):
    fixture_path = tmp_path / "recorded.eggr"

    async def run():
        async def upstream(reader, writer):
            await reader.readuntil(b"\r\n\r\n")
            writer.write(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            await writer.drain()
            writer.close()
            await writer.wait_closed()

        origin = await asyncio.start_server(upstream, "127.0.0.1", 0)
        port = origin.sockets[0].getsockname()[1]
        try:
            server = await eggreplay.recording_gateway(
                str(fixture_path), f"http://127.0.0.1:{port}"
            )
            response = await raw_request(server.address)
            assert b"200" in response and response.endswith(b"ok")
            close_task = asyncio.ensure_future(server.aclose())
            await asyncio.sleep(0)
            close_task.cancel()
            try:
                await close_task
            except asyncio.CancelledError:
                pass
            for _ in range(500):
                if fixture_path.is_dir():
                    break
                await asyncio.sleep(0.01)
            assert fixture_path.is_dir()
            assert eggreplay.Fixture(str(fixture_path)).manifest["flow_count"] == 1
            appended = await eggreplay.recording_gateway(
                str(fixture_path), f"http://127.0.0.1:{port}", record_mode="append-new"
            )
            response = await raw_request(appended.address, path="/new")
            assert response.endswith(b"ok")
            await appended.aclose()
            assert eggreplay.Fixture(str(fixture_path)).manifest["flow_count"] == 2

            rerecorded = await eggreplay.recording_gateway(
                str(fixture_path), f"http://127.0.0.1:{port}", record_mode="re-record"
            )
            assert (await raw_request(rerecorded.address)).endswith(b"ok")
            await rerecorded.aclose()
            assert eggreplay.Fixture(str(fixture_path)).manifest["flow_count"] == 1

            sealed = await eggreplay.recording_gateway(
                str(fixture_path), f"http://127.0.0.1:{port}", record_mode="once"
            )
            assert b"404" in await raw_request(sealed.address)
            await sealed.aclose()
            assert eggreplay.Fixture(str(fixture_path)).manifest["flow_count"] == 1

            independent_paths = [tmp_path / "parallel-a.eggr", tmp_path / "parallel-b.eggr"]
            independent = [
                await eggreplay.recording_gateway(
                    str(path), f"http://127.0.0.1:{port}"
                )
                for path in independent_paths
            ]
            responses = await asyncio.gather(
                *(raw_request(server.address) for server in independent)
            )
            assert all(response.endswith(b"ok") for response in responses)
            await asyncio.gather(*(server.aclose() for server in independent))
            assert [
                eggreplay.Fixture(str(path)).manifest["flow_count"]
                for path in independent_paths
            ] == [1, 1]
        finally:
            origin.close()
            await origin.wait_closed()

    asyncio.run(run())


def test_candidate_regression_uses_rust_report_and_leaves_event_loop_live(tmp_path):
    root, _ = write_fixture(tmp_path / "regression.eggr")
    flow = json.loads((root / "flows.jsonl").read_text())
    flow["request"]["headers"].extend(
        [{"name": "content-length", "value": "4"}, {"name": "connection", "value": "close"}]
    )
    (root / "flows.jsonl").write_text(json.dumps(flow) + "\n")

    async def run():
        heartbeats = 0

        async def upstream(reader, writer):
            nonlocal heartbeats
            headers = await reader.readuntil(b"\r\n\r\n")
            length = next(
                int(line.split(b":", 1)[1])
                for line in headers.split(b"\r\n")
                if line.lower().startswith(b"content-length:")
            )
            await reader.readexactly(length)
            await asyncio.sleep(0.1)
            writer.write(
                b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nbody"
            )
            await writer.drain()
            writer.close()
            await writer.wait_closed()

        async def heartbeat():
            nonlocal heartbeats
            while True:
                heartbeats += 1
                await asyncio.sleep(0.005)

        async def proxy(reader, writer):
            request_line = await reader.readline()
            method, authority, _version = request_line.decode().split()
            assert method == "CONNECT"
            while await reader.readline() != b"\r\n":
                pass
            host, port = authority.rsplit(":", 1)
            upstream_reader, upstream_writer = await asyncio.open_connection(host, int(port))
            writer.write(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            await writer.drain()

            async def pipe(source, destination):
                try:
                    while data := await source.read(65536):
                        destination.write(data)
                        await destination.drain()
                finally:
                    destination.close()

            await asyncio.gather(
                pipe(reader, upstream_writer), pipe(upstream_reader, writer)
            )

        origin = await asyncio.start_server(upstream, "127.0.0.1", 0)
        port = origin.sockets[0].getsockname()[1]
        proxy_server = await asyncio.start_server(proxy, "127.0.0.1", 0)
        proxy_port = proxy_server.sockets[0].getsockname()[1]
        ticker = asyncio.create_task(heartbeat())
        try:
            report = await eggreplay.regress_flow(
                eggreplay.Fixture(str(root)),
                "flow-1",
                f"http://127.0.0.1:{port}",
                compare_stream_events=True,
            )
            assert report.baseline_flow_ids == ["flow-1"]
            assert report.finding_count > 0
            assert heartbeats >= 5
            routed = await eggreplay.regress_flow(
                eggreplay.Fixture(str(root)),
                "flow-1",
                f"http://127.0.0.1:{port}",
                route=f"http://127.0.0.1:{proxy_port}",
            )
            assert routed.baseline_flow_ids == ["flow-1"]
        finally:
            ticker.cancel()
            origin.close()
            await origin.wait_closed()
            proxy_server.close()
            await proxy_server.wait_closed()

    asyncio.run(run())


def test_websocket_candidate_uses_rust_websocket_regression_authority(tmp_path):
    root, _ = write_fixture(tmp_path / "websocket.eggr", with_extensions=True)
    flow = json.loads((root / "flows.jsonl").read_text())
    flow["request"].update(
        {
            "method": "GET",
            "body": {"kind": "absent"},
            "headers": [
                {"name": "connection", "value": "Upgrade"},
                {"name": "upgrade", "value": "websocket"},
                {"name": "sec-websocket-key", "value": "dGhlIHNhbXBsZSBub25jZQ=="},
                {"name": "sec-websocket-version", "value": "13"},
            ],
        }
    )
    flow["request"].pop("authority", None)
    flow["request"]["authority"] = "example.test"
    flow["outcome"]["status"] = 101
    (root / "flows.jsonl").write_text(json.dumps(flow) + "\n")
    (root / "websockets.jsonl").write_text(
        json.dumps(
            {
                "schema_version": 1,
                "conversations": [
                    {
                        "id": "ws-1",
                        "flow_id": "flow-1",
                        "offered_subprotocols": [],
                        "messages": [],
                        "terminal": {"kind": "abnormal", "cause": "eof"},
                    }
                ],
            }
        )
    )

    async def run():
        async def reject(reader, writer):
            await reader.readuntil(b"\r\n\r\n")
            writer.write(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            await writer.drain()
            writer.close()
            await writer.wait_closed()

        candidate = await asyncio.start_server(reject, "127.0.0.1", 0)
        port = candidate.sockets[0].getsockname()[1]
        try:
            report = await eggreplay.regress_flow(
                eggreplay.Fixture(str(root)), "flow-1", f"http://127.0.0.1:{port}"
            )
            assert report.baseline_flow_ids == ["flow-1"]
            assert report.finding_count > 0
        finally:
            candidate.close()
            await candidate.wait_closed()

    asyncio.run(run())


def test_candidate_cancellation_closes_pending_network_request(tmp_path):
    root, _ = write_fixture(tmp_path / "cancel-regression.eggr")

    async def run():
        accepted = asyncio.Event()
        disconnected = asyncio.Event()

        async def stalled(reader, writer):
            await reader.readuntil(b"\r\n\r\n")
            accepted.set()
            await reader.read()
            disconnected.set()
            writer.close()
            await writer.wait_closed()

        candidate = await asyncio.start_server(stalled, "127.0.0.1", 0)
        port = candidate.sockets[0].getsockname()[1]
        try:
            task = asyncio.ensure_future(
                eggreplay.regress_flow(
                    eggreplay.Fixture(str(root)),
                    "flow-1",
                    f"http://127.0.0.1:{port}",
                )
            )
            await asyncio.wait_for(accepted.wait(), 1)
            task.cancel()
            with pytest.raises(asyncio.CancelledError):
                await task
            await asyncio.wait_for(disconnected.wait(), 1)
        finally:
            candidate.close()
            await candidate.wait_closed()

    asyncio.run(run())


def test_pytest_plugin_sync_async_and_decorator_replay(tmp_path, pytester):
    root, _ = write_fixture(tmp_path / "plugin.eggr")
    flow = json.loads((root / "flows.jsonl").read_text())
    flow["request"]["headers"].extend(
        [{"name": "content-length", "value": "4"}, {"name": "connection", "value": "close"}]
    )
    (root / "flows.jsonl").write_text(json.dumps(flow) + "\n")
    pytester.makepyfile(
        test_plugin=f"""
import pytest
import eggreplay

def test_sync_fixture(eggreplay_server, eggreplay_fixture):
    assert eggreplay_fixture.manifest["flow_count"] == 1
    assert eggreplay_server.address.startswith("127.0.0.1:")

@pytest.mark.asyncio
async def test_async_fixture(eggreplay_async_server):
    assert eggreplay_async_server.address.startswith("127.0.0.1:")

@eggreplay.use_fixture({str(root)!r})
def test_vcr_decorator(eggreplay_server):
    assert eggreplay_server.address
"""
    )
    result = pytester.runpytest("--eggreplay-fixture", str(root), "-q")
    result.assert_outcomes(passed=3)


def test_pytest_plugin_missing_fixture_is_read_only(tmp_path, pytester):
    absent = tmp_path / "missing.eggr"
    pytester.makepyfile("def test_missing(eggreplay_server): pass")
    result = pytester.runpytest("--eggreplay-fixture", str(absent), "-q")
    result.assert_outcomes(errors=1)
    assert not absent.exists()
    assert not absent.with_name(f".{absent.name}.eggreplay.lock").exists()


def test_pytest_plugin_generic_update_flag_does_not_enable_writes(tmp_path, pytester):
    absent = tmp_path / "sealed.eggr"
    pytester.makepyfile("def test_missing(eggreplay_server): pass")
    result = pytester.runpytest("--eggreplay-fixture", str(absent), "--update", "-q")
    assert result.ret != 0
    assert "unrecognized arguments: --update" in result.stderr.str()
    assert not absent.exists()


def test_read_only_workers_can_share_fixture_concurrently(tmp_path):
    root, _ = write_fixture(tmp_path / "shared.eggr")
    flow = json.loads((root / "flows.jsonl").read_text())
    flow["request"]["headers"].extend(
        [{"name": "content-length", "value": "4"}, {"name": "connection", "value": "close"}]
    )
    (root / "flows.jsonl").write_text(json.dumps(flow) + "\n")

    async def run():
        fixture = eggreplay.Fixture(str(root))
        servers = await asyncio.gather(
            eggreplay.replay_server(fixture), eggreplay.replay_server(fixture)
        )
        try:
            responses = await asyncio.gather(
                *(raw_request(server.address) for server in servers)
            )
            assert all(b"200" in response for response in responses)
        finally:
            await asyncio.gather(*(server.aclose() for server in servers))

    asyncio.run(run())


def test_pytest_plugin_once_is_explicit_and_seals_after_creation(tmp_path, pytester):
    fixture_path = tmp_path / "once.eggr"
    pytester.makepyfile("def test_recording(eggreplay_server): assert eggreplay_server.address")
    result = pytester.runpytest(
        "--eggreplay-fixture",
        str(fixture_path),
        "--eggreplay-record-mode=once",
        "--eggreplay-upstream=http://127.0.0.1:9",
        "-q",
    )
    result.assert_outcomes(passed=1)
    assert eggreplay.Fixture(str(fixture_path)).manifest["flow_count"] == 0
    assert not fixture_path.with_name(f".{fixture_path.name}.eggreplay.lock").exists()


def test_pytest_plugin_append_and_rerecord_require_explicit_modes(tmp_path, pytester):
    fixture_path, _ = write_fixture(tmp_path / "append-plugin.eggr")
    pytester.makepyfile("def test_server(eggreplay_server): assert eggreplay_server.address")
    appended = pytester.runpytest(
        "--eggreplay-fixture",
        str(fixture_path),
        "--eggreplay-record-mode=append-new",
        "--eggreplay-upstream=http://127.0.0.1:9",
        "-q",
    )
    appended.assert_outcomes(passed=1)
    assert eggreplay.Fixture(str(fixture_path)).manifest["flow_count"] == 1
    assert not fixture_path.with_name(f".{fixture_path.name}.eggreplay.lock").exists()

    rerecorded = pytester.runpytest(
        "--eggreplay-fixture",
        str(fixture_path),
        "--eggreplay-record-mode=re-record",
        "--eggreplay-upstream=http://127.0.0.1:9",
        "-q",
    )
    rerecorded.assert_outcomes(passed=1)
    assert eggreplay.Fixture(str(fixture_path)).manifest["flow_count"] == 0


def test_pytest_plugin_surfaces_websocket_append_limitation(tmp_path, pytester):
    fixture_path, _ = write_fixture(tmp_path / "ws-append.eggr")
    pytester.makepyfile("def test_server(eggreplay_server): pass")
    result = pytester.runpytest(
        "--eggreplay-fixture",
        str(fixture_path),
        "--eggreplay-record-mode=append-new",
        "--eggreplay-upstream=http://127.0.0.1:9",
        "--eggreplay-websockets",
        "-q",
    )
    result.assert_outcomes(errors=1)
    assert "WebSocket append-new is not supported" in result.stdout.str()
    assert fixture_path.is_dir()
    assert not fixture_path.with_name(f".{fixture_path.name}.eggreplay.lock").exists()


def test_pytest_writer_lock_refuses_duplicate_and_allows_independent_paths(tmp_path):
    from eggreplay.pytest_plugin import _WriterLock

    first = _WriterLock(tmp_path / "same.eggr", "gw0")
    with first:
        with pytest.raises(RuntimeError, match="already has a writer lock"):
            with _WriterLock(tmp_path / "same.eggr", "gw1"):
                pass
        with _WriterLock(tmp_path / "worker-a.eggr", "gw0"):
            with _WriterLock(tmp_path / "worker-b.eggr", "gw1"):
                pass
    assert not first.path.exists()


def test_vcr_style_sync_and_async_contexts_share_server_lifecycle(tmp_path):
    root, _ = write_fixture(tmp_path / "vcr.eggr")
    with eggreplay.fixture_context(root) as server:
        assert server.address.startswith("127.0.0.1:")

    async def run():
        async with eggreplay.fixture_context(root) as server:
            assert server.address.startswith("127.0.0.1:")

    asyncio.run(run())
