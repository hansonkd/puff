"""
Benchmark WSGI app for testing free-threaded Python performance.

Two endpoints:
  /cpu  - CPU-bound work (fibonacci) to show GIL vs free-threaded difference
  /io   - Simulated I/O (sleep) to show baseline async performance
"""
import json
import time


def fibonacci(n):
    """CPU-bound work that holds the GIL under regular Python."""
    if n < 2:
        return n
    a, b = 0, 1
    for _ in range(n - 1):
        a, b = b, a + b
    return b


def application(environ, start_response):
    path = environ.get("PATH_INFO", "/")

    if path == "/cpu":
        # CPU-bound: compute fibonacci(10000) — enough to stress the GIL
        t0 = time.monotonic()
        result = fibonacci(10000)
        elapsed_us = int((time.monotonic() - t0) * 1_000_000)
        body = json.dumps({
            "type": "cpu",
            "fib_digits": len(str(result)),
            "elapsed_us": elapsed_us,
        }).encode()
    elif path == "/io":
        # Simulated I/O: 1ms sleep
        t0 = time.monotonic()
        time.sleep(0.001)
        elapsed_us = int((time.monotonic() - t0) * 1_000_000)
        body = json.dumps({
            "type": "io",
            "elapsed_us": elapsed_us,
        }).encode()
    else:
        body = b'{"status": "ok"}'

    start_response("200 OK", [
        ("Content-Type", "application/json"),
        ("Content-Length", str(len(body))),
    ])
    return [body]
