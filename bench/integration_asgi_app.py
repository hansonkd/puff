"""Minimal ASGI benchmark app for Puff integration profiling."""
from __future__ import annotations

import asyncio

import puff


REDIS_KEY = "bench:key"
POSTGRES_SQL = "SELECT value FROM puff_bench_lookup WHERE id = 1"


def _to_bytes(value) -> bytes:
    if value is None:
        return b""
    if isinstance(value, bytes):
        return value
    if isinstance(value, bytearray):
        return bytes(value)
    return str(value).encode("utf-8")


async def _call_puff(method, *args):
    loop = asyncio.get_running_loop()
    result = loop.create_future()

    def done(value, error):
        if error is None:
            loop.call_soon_threadsafe(result.set_result, value)
        else:
            loop.call_soon_threadsafe(result.set_exception, error)

    method(done, *args)
    return await result


async def _postgres_lookup() -> bytes:
    conn = puff.rust_objects.global_postgres_getter()
    try:
        await _call_puff(conn.set_auto_commit, True)
        cursor = conn.cursor()
        result = await _call_puff(cursor.execute, POSTGRES_SQL, None)
        rows = result[0] if isinstance(result, tuple) and len(result) == 3 else result
        return _to_bytes(rows[0][0])
    finally:
        conn.close()


async def _redis_lookup() -> bytes:
    client = puff.rust_objects.global_redis_getter()
    return _to_bytes(await _call_puff(client.get, REDIS_KEY))


async def application(scope, receive, send):
    if scope["type"] == "lifespan":
        while True:
            message = await receive()
            if message["type"] == "lifespan.startup":
                await send({"type": "lifespan.startup.complete"})
            elif message["type"] == "lifespan.shutdown":
                await send({"type": "lifespan.shutdown.complete"})
                return
        return

    if scope["type"] != "http":
        return

    path = scope.get("path", "/")
    status = 200

    if path == "/baseline":
        body = b"ok"
    elif path == "/redis":
        body = await _redis_lookup()
    elif path == "/pg":
        body = await _postgres_lookup()
    elif path == "/combo":
        body = await _redis_lookup()
        body += b":" + await _postgres_lookup()
    else:
        status = 404
        body = b"not-found"

    await send(
        {
            "type": "http.response.start",
            "status": status,
            "headers": [
                [b"content-type", b"application/octet-stream"],
                [b"content-length", str(len(body)).encode("ascii")],
            ],
        }
    )
    await send({"type": "http.response.body", "body": body})
