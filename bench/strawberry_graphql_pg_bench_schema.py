from __future__ import annotations

import os

import psycopg2.pool
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


_POOL = psycopg2.pool.ThreadedConnectionPool(
    minconn=1,
    maxconn=32,
    dsn=os.environ["BENCH_PG_DSN"],
)


def _fetch(count: int) -> list[PgBenchRow]:
    conn = _POOL.getconn()
    try:
        conn.autocommit = True
        with conn.cursor() as cursor:
            cursor.execute(POSTGRES_SQL, [count])
            rows = cursor.fetchall()
        return [PgBenchRow(id=row[0], value=row[1]) for row in rows]
    finally:
        _POOL.putconn(conn)


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
