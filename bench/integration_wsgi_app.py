"""Minimal WSGI benchmark app for Puff integration profiling.

Endpoints:
  /baseline  - fixed in-process response
  /redis     - single Redis GET
  /pg        - single Postgres SELECT
  /combo     - Redis GET followed by Postgres SELECT
"""
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


def _maybe_sync(value):
    if inspect.isawaitable(value):
        raise RuntimeError("WSGI benchmark app received an awaitable")
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


def _postgres_lookup() -> bytes:
    conn = _new_postgres_connection()
    try:
        set_auto_commit = getattr(conn, "set_auto_commit", None)
        if callable(set_auto_commit):
            _maybe_sync(set_auto_commit(True))
        cursor = conn.cursor()
        result = _maybe_sync(cursor.execute(POSTGRES_SQL))
        if isinstance(result, tuple) and len(result) == 3:
            rows = result[0]
        else:
            rows = _maybe_sync(cursor.fetchall())
        return _to_bytes(rows[0][0])
    finally:
        close = getattr(conn, "close", None)
        if callable(close):
            _maybe_sync(close())


def _redis_lookup() -> bytes:
    return _to_bytes(_maybe_sync(global_redis.get(REDIS_KEY)))


def application(environ, start_response):
    path = environ.get("PATH_INFO", "/")

    if path == "/baseline":
        body = b"ok"
    elif path == "/redis":
        body = _redis_lookup()
    elif path == "/pg":
        body = _postgres_lookup()
    elif path == "/combo":
        body = _redis_lookup() + b":" + _postgres_lookup()
    else:
        body = b"not-found"

    status = "200 OK" if path in {"/baseline", "/redis", "/pg", "/combo"} else "404 Not Found"
    start_response(
        status,
        [
            ("Content-Type", "application/octet-stream"),
            ("Content-Length", str(len(body))),
        ],
    )
    return [body]
