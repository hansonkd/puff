from dataclasses import dataclass
from typing import Any, Optional

from puff import postgres
from puff.graphql import EmptyObject


POSTGRES_SQL = """
SELECT id, value
FROM puff_graphql_pg_bench
WHERE id <= $1
ORDER BY id
"""


@dataclass
class PgBenchRow:
    id: int
    value: str


def _python_fetch(count: int) -> list[PgBenchRow]:
    conn = postgres.connect(autocommit=True)
    try:
        cursor = conn.cursor()
        cursor.execute(POSTGRES_SQL, [count])
        rows = cursor.fetchall()
        return [PgBenchRow(id=row[0], value=row[1]) for row in rows]
    finally:
        conn.close()


@dataclass
class Query:
    @classmethod
    def pg_items_fused(
        cls, context, /, count: Optional[int] = 64
    ) -> tuple[list[PgBenchRow], str, list[Any]]:
        return ..., POSTGRES_SQL, [count or 64]

    @classmethod
    def pg_items_python(
        cls, context, /, count: Optional[int] = 64
    ) -> tuple[list[PgBenchRow], list[Any]]:
        return ..., _python_fetch(count or 64)


@dataclass
class Schema:
    query: Query
    mutation: EmptyObject
    subscription: EmptyObject
