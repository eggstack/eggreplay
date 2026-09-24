import asyncio

import eggreplay
from eggreplay import _native


def test_native_import_and_roundtrip():
    assert _native.version() == "0.1.0"


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
