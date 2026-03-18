"""
Benchmark ASGI app — same CPU workload as wsgi_app.py but for uvicorn.
"""
import json
import time


def fibonacci(n):
    """CPU-bound work."""
    if n < 2:
        return n
    a, b = 0, 1
    for _ in range(n - 1):
        a, b = b, a + b
    return b


async def application(scope, receive, send):
    if scope["type"] != "http":
        return

    path = scope.get("path", "/")

    if path == "/cpu":
        t0 = time.monotonic()
        result = fibonacci(10000)
        elapsed_us = int((time.monotonic() - t0) * 1_000_000)
        body = json.dumps({
            "type": "cpu",
            "fib_digits": len(str(result)),
            "elapsed_us": elapsed_us,
        }).encode()
    else:
        body = b'{"status": "ok"}'

    await send({
        "type": "http.response.start",
        "status": 200,
        "headers": [
            [b"content-type", b"application/json"],
            [b"content-length", str(len(body)).encode()],
        ],
    })
    await send({
        "type": "http.response.body",
        "body": body,
    })
