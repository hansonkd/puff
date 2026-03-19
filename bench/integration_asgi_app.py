"""Minimal ASGI benchmark app for Puff integration profiling."""
from __future__ import annotations

import inspect

import puff
from puff.redis import global_redis


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


async def _maybe_await(value):
    if inspect.isawaitable(value):
        return await value
    return value


def _new_postgres_connection():
    postgres_api = getattr(puff, "postgres", None)
    if callable(postgres_api):
        return postgres_api()
    if postgres_api is not None:
        for name in ("PostgresConnection", "Connection", "connect"):
            constructor = getattr(postgres_api, name, None)
            if constructor is not None:
                return constructor()
    raise RuntimeError("Puff Postgres API is not available")


async def _postgres_lookup() -> bytes:
    conn = _new_postgres_connection()
    try:
        set_auto_commit = getattr(conn, "set_auto_commit", None)
        if callable(set_auto_commit):
            await _maybe_await(set_auto_commit(True))
        cursor = conn.cursor()
        result = await _maybe_await(cursor.execute(POSTGRES_SQL))
        if isinstance(result, tuple) and len(result) == 3:
            rows = result[0]
        else:
            rows = await _maybe_await(cursor.fetchall())
        return _to_bytes(rows[0][0])
    finally:
        close = getattr(conn, "close", None)
        if callable(close):
            await _maybe_await(close())


async def _redis_lookup() -> bytes:
    return _to_bytes(await _maybe_await(global_redis.get(REDIS_KEY)))


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
        body = await _redis_lookup() + b":" + await _postgres_lookup()
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
