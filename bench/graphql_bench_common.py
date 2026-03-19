from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True)
class BenchItemData:
    value: int
    text: str


def build_items(count: int) -> list[BenchItemData]:
    return [BenchItemData(value=i, text=f"item-{i}") for i in range(count)]


def make_child(item: BenchItemData, multiplier: int) -> BenchItemData:
    return BenchItemData(value=item.value * multiplier, text=f"{item.text}-child")
