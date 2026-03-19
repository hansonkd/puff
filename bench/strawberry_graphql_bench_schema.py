from __future__ import annotations

import strawberry
from strawberry.asgi import GraphQL
from strawberry.schema.config import StrawberryConfig

from bench.graphql_bench_common import BenchItemData, build_items, make_child


@strawberry.type
class StrawberryBenchItem:
    value: int
    text: str

    @classmethod
    def from_data(cls, item: BenchItemData) -> "StrawberryBenchItem":
        return cls(value=item.value, text=item.text)

    @strawberry.field
    def label(self) -> str:
        return f"{self.value}:{self.text}"

    @strawberry.field
    def child(self, multiplier: int = 2) -> "StrawberryBenchItem":
        return self.from_data(make_child(BenchItemData(self.value, self.text), multiplier))


def _make_strawberry_field(ix: int):
    @strawberry.field
    def resolver(self) -> str:
        return f"value-{ix}"

    resolver.__name__ = f"field_{ix}"
    return resolver


@strawberry.type
class StrawberryBenchQuery:
    @strawberry.field
    def hello_world(self) -> str:
        return "hello-world"

    @strawberry.field
    def items(self, count: int = 64) -> list[StrawberryBenchItem]:
        return [StrawberryBenchItem.from_data(item) for item in build_items(count)]
    for _ix in range(64):
        locals()[f"field_{_ix}"] = _make_strawberry_field(_ix)
    del _ix


strawberry_schema = strawberry.Schema(
    query=StrawberryBenchQuery,
    config=StrawberryConfig(auto_camel_case=False),
)
strawberry_app = GraphQL(strawberry_schema)
