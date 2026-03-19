from __future__ import annotations

import os
from urllib.parse import urlparse

import pg8000
import strawberry
from strawberry.asgi import GraphQL
from strawberry.schema.config import StrawberryConfig


POSTGRES_SQL = """
SELECT id, value
FROM puff_graphql_pg_bench
WHERE id <= %s
ORDER BY id
"""


@strawberry.type
class PgBenchRow:
    id: int
    value: str

_parsed_dsn = urlparse(os.environ["BENCH_PG_DSN"])


def _fetch(count: int) -> list[PgBenchRow]:
    conn = pg8000.connect(
        user=_parsed_dsn.username,
        password=_parsed_dsn.password,
        host=_parsed_dsn.hostname or "127.0.0.1",
        port=_parsed_dsn.port or 5432,
        database=_parsed_dsn.path.lstrip("/") or None,
    )
    try:
        with conn.cursor() as cursor:
            cursor.execute(POSTGRES_SQL, [count])
            rows = cursor.fetchall()
        conn.commit()
        return [PgBenchRow(id=row[0], value=row[1]) for row in rows]
    finally:
        conn.close()


@strawberry.type
class Query:
    @strawberry.field
    def pg_items(self, count: int = 64) -> list[PgBenchRow]:
        return _fetch(count)


strawberry_schema = strawberry.Schema(
    query=Query,
    config=StrawberryConfig(auto_camel_case=False),
)
strawberry_app = GraphQL(strawberry_schema)
