from dataclasses import dataclass
from typing import Optional, Tuple, List, Any, Iterable
from puff.graphql import EmptyObject


@dataclass
class DbObject:
    count: int
    parent_id: int
    title: str


@dataclass
class SomeInputObject:
    some_count: int
    some_string: str


@dataclass
class SomeObject:
    field1: int
    field2: str

    @classmethod
    def hello_world2(cls, ctx, /, my_input: Optional[int] = None) -> "SomeObject":
        return SomeObject(field1=my_input, field2=f"hello: {my_input}")

    @classmethod
    def hello_world_query(
        cls, ctx, /, my_input: Optional[int] = None
    ) -> Tuple[List[DbObject], str, List[Any], List[str], List[str]]:
        q = "SELECT ($2 || ' HELLO WORLD '::TEXT || $1) AS title, unnest($3::int[]) AS count, unnest($3::int[]) % 2 as parent_id"
        return ..., q, ["cowabunga", "uwu", [5, 6, 7, 8, 42]], ["field1"], ["parent_id"]


@dataclass
class Query:
    @classmethod
    def hello_world(cls, context, /, my_input: Optional[int] = None) -> str:
        return f"hello: {my_input}"

    @classmethod
    def hello_world_object(cls, ctx, /) -> "SomeObject":
        return SomeObject(field1=42, field2=f"hello: single")

    @classmethod
    def hello_world_objects(
        cls, context, /
    ) -> Tuple[List[SomeObject], List[Any]]:
        objs = [
            SomeObject(field1=8, field2="fa three"),
            SomeObject(field1=42, field2="fa so la de do"),
            SomeObject(field1=0, field2="fa so la de do"),
            SomeObject(field1=5, field2="None"),
            SomeObject(field1=1, field2="reda"),
        ]
        return ..., objs


@dataclass
class Mutation:
    @classmethod
    def hello_world(cls, context, /, my_input: Optional[int] = None) -> str:
        return f"hello: {my_input}"


@dataclass
class Subscription:
    @classmethod
    def read_some_objects(
        cls, context, /, num: Optional[int] = None
    ) -> Iterable[SomeObject]:
        for ix in range(num or 1):
            yield SomeObject(field1=ix, field2=f"item {ix} of {num}")

    @classmethod
    def async_read_some_objects(
        cls, context, /, num: Optional[int] = None
    ) -> Iterable[SomeObject]:
        for ix in range(num or 1):
            yield SomeObject(field1=ix, field2=f"item {ix} of {num}")


@dataclass
class Schema:
    query: Query
    mutation: Mutation
    subscription: Subscription


@dataclass
class AltQuery:
    @classmethod
    def alt_hello_world(cls, context, /) -> str:
        return "hello from alternate"


@dataclass
class AltSchema:
    query: AltQuery
    mutation: EmptyObject
    subscription: EmptyObject
