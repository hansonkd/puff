from dataclasses import dataclass
from typing import Optional, Tuple, List, Any
from puff import graphql


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
    def hello_world2(cls, ctx, ids, /, my_input: Optional[int] = None) -> "SomeObject":
        return SomeObject(field1=my_input, field2=f"hello: {my_input}")

    @classmethod
    def hello_world_query(
        cls, ctx, ids, /, my_input: Optional[int] = None
    ) -> Tuple[List[DbObject], str, List[Any], List[str], List[str]]:
        q = "SELECT ($2 || ' HELLO WORLD '::TEXT || $1) AS title, unnest($3::int[]) AS count, unnest($3::int[]) % 2 as parent_id"
        return ..., q, ["cowabunga", "uwu", [5, 6, 7, 8]], ["field1"], ["parent_id"]


@dataclass
class Query:
    @classmethod
    def hello_world(cls, parents, context, /, my_input: Optional[int] = None) -> str:
        return f"hello: {my_input}"

    @classmethod
    def hello_world_object(
        cls, parents, context, /, my_input: List[SomeInputObject]
    ) -> Tuple[List[SomeObject], List[Any]]:
        objs = [
            SomeObject(field1=0, field2="fa three"),
            SomeObject(field1=0, field2="fa so la de do"),
            SomeObject(field1=5, field2="None"),
            SomeObject(field1=1, field2="reda"),
        ]
        if my_input:
            for inp in my_input:
                objs.append(SomeObject(field1=inp.some_count, field2=inp.some_string))
        return ..., objs


@dataclass
class Mutation:
    @classmethod
    def hello_world(cls, parents, context, /, my_input: Optional[int] = None) -> str:
        return f"hello: {my_input}"


@dataclass
class Subscription:
    @staticmethod
    def __accept_hello_world(render_data):
        for x in range(0, 3):
            print("sending")
            r = render_data(None)
            print(f"done sending {r}")

    @classmethod
    @graphql.acceptor(__accept_hello_world)
    def hello_world(
        cls, parents, context, /, my_input: Optional[int] = None
    ) -> List[str]:
        return [f"hello: 1 {my_input}", f"hello: 2 {my_input}"]

    @classmethod
    @graphql.acceptor(__accept_hello_world)
    def hello_world_object(
        cls, parents, context, /, my_input: List[SomeInputObject]
    ) -> Tuple[List[SomeObject], List[Any]]:
        objs = [
            SomeObject(field1=0, field2="fa three"),
            SomeObject(field1=0, field2="fa so la de do"),
            SomeObject(field1=5, field2="None"),
            SomeObject(field1=1, field2="reda"),
        ]
        if my_input:
            for inp in my_input:
                objs.append(SomeObject(field1=inp.some_count, field2=inp.some_string))
        return ..., objs


@dataclass
class Schema:
    query: Query
    mutation: Mutation
    subscription: Subscription


def schema():
    return Schema
