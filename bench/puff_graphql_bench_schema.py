from dataclasses import dataclass
from typing import Optional

from bench.graphql_bench_common import BenchItemData, build_items, make_child


@dataclass
class EmptyObject:
    pass


@dataclass
class PuffBenchItem:
    value: int
    text: str

    @classmethod
    def _from_data(cls, item: BenchItemData) -> "PuffBenchItem":
        return cls(value=item.value, text=item.text)

    def label(self, context, /) -> str:
        return f"{self.value}:{self.text}"

    def child(self, context, /, multiplier: Optional[int] = 2) -> "PuffBenchItem":
        return self._from_data(make_child(BenchItemData(self.value, self.text), multiplier or 2))


@dataclass
class PuffBenchQuery:
    @classmethod
    def hello_world(cls, context, /) -> str:
        return "hello-world"

    @classmethod
    def items(cls, context, /, count: Optional[int] = 64) -> list[PuffBenchItem]:
        return [PuffBenchItem._from_data(item) for item in build_items(count or 64)]


def _make_puff_field(ix: int):
    @classmethod
    def field(cls, context, /) -> str:
        return f"value-{ix}"

    field.__name__ = f"field_{ix}"
    return field


for _ix in range(64):
    setattr(PuffBenchQuery, f"field_{_ix}", _make_puff_field(_ix))


@dataclass
class PuffBenchSchema:
    query: PuffBenchQuery
    mutation: EmptyObject
    subscription: EmptyObject
