"""Auto-CRUD GraphQL schema generation for Puff.

The @model decorator inspects a dataclass and generates SQL queries for
list, get-by-id, create, update, and delete operations. These queries are
attached to the class via __puff_crud__ and can be turned into Query/Mutation
dataclasses with generate_crud_schema().

Usage:
    from dataclasses import dataclass
    from puff.model import model, generate_crud_schema

    @model(table="customers")
    @dataclass
    class Customer:
        id: int
        name: str
        email: str
        created_at: str

    Query, Mutation = generate_crud_schema(Customer)

    @dataclass
    class Schema:
        query: Query
        mutation: Mutation
"""
from dataclasses import dataclass, fields as dc_fields
from typing import List, Tuple, Any, Optional


# Type mapping: Python type -> (SQL type, GraphQL nullable)
TYPE_MAP = {
    int: ("INT", False),
    str: ("TEXT", False),
    float: ("FLOAT8", False),
    bool: ("BOOLEAN", False),
}


def model(table: str, primary_key: str = "id"):
    """Auto-generate GraphQL CRUD for a dataclass model.

    Args:
        table: The Postgres table name.
        primary_key: The primary key column (default: "id").

    The decorator attaches several attributes to the class:
        __puff_table__        - table name
        __puff_primary_key__  - primary key column
        __puff_crud__         - dict of CRUD operation descriptors
        __puff_create_table__ - CREATE TABLE IF NOT EXISTS DDL
    """
    def decorator(cls):
        model_fields = dc_fields(cls)
        field_names = [f.name for f in model_fields]
        non_pk_fields = [f for f in model_fields if f.name != primary_key]

        columns = ", ".join(field_names)

        # SELECT all
        select_all = f"SELECT {columns} FROM {table}"

        # SELECT by primary key
        select_by_id = f"SELECT {columns} FROM {table} WHERE {primary_key} = $1"

        # INSERT: non-pk fields only (pk is assumed SERIAL)
        insert_cols = ", ".join(f.name for f in non_pk_fields)
        insert_params = ", ".join(f"${i+1}" for i in range(len(non_pk_fields)))
        insert_sql = (
            f"INSERT INTO {table} ({insert_cols}) "
            f"VALUES ({insert_params}) RETURNING {columns}"
        )

        # UPDATE: SET all non-pk fields, WHERE pk = $N
        set_clauses = ", ".join(
            f"{f.name} = ${i+1}" for i, f in enumerate(non_pk_fields)
        )
        pk_param = f"${len(non_pk_fields) + 1}"
        update_sql = (
            f"UPDATE {table} SET {set_clauses} "
            f"WHERE {primary_key} = {pk_param} RETURNING {columns}"
        )

        # DELETE
        delete_sql = f"DELETE FROM {table} WHERE {primary_key} = $1"

        # Store on the class
        cls.__puff_table__ = table
        cls.__puff_primary_key__ = primary_key
        cls.__puff_crud__ = {
            "list": {"sql": select_all, "args": []},
            "get": {"sql": select_by_id, "args": [primary_key]},
            "create": {
                "sql": insert_sql,
                "args": [f.name for f in non_pk_fields],
            },
            "update": {
                "sql": update_sql,
                "args": [f.name for f in non_pk_fields] + [primary_key],
            },
            "delete": {"sql": delete_sql, "args": [primary_key]},
        }

        # Generate migration DDL
        col_defs = []
        for f in model_fields:
            sql_type = TYPE_MAP.get(f.type, ("TEXT", False))[0]
            if f.name == primary_key:
                col_defs.append(f"{f.name} SERIAL PRIMARY KEY")
            else:
                col_defs.append(f"{f.name} {sql_type}")
        cls.__puff_create_table__ = (
            f"CREATE TABLE IF NOT EXISTS {table} ({', '.join(col_defs)})"
        )

        return cls

    return decorator


def generate_crud_schema(*models):
    """Generate Query and Mutation dataclasses from @model classes.

    Returns (QueryClass, MutationClass) that can be used as the GraphQL schema.

    Usage:
        Query, Mutation = generate_crud_schema(Customer, Order, Product)

        @dataclass
        class Schema:
            query: Query
            mutation: Mutation
    """
    query_methods = {}
    mutation_methods = {}

    for model_cls in models:
        crud = model_cls.__puff_crud__
        name = model_cls.__name__.lower()

        # List all
        list_fn = _make_crud_method(f"{name}s", crud["list"])
        query_methods[f"{name}s"] = list_fn

        # Get by ID
        get_fn = _make_crud_method(f"{name}_by_id", crud["get"])
        query_methods[f"{name}_by_id"] = get_fn

        # Create
        create_fn = _make_crud_method(f"create_{name}", crud["create"])
        mutation_methods[f"create_{name}"] = create_fn

        # Update
        update_fn = _make_crud_method(f"update_{name}", crud["update"])
        mutation_methods[f"update_{name}"] = update_fn

        # Delete
        delete_fn = _make_crud_method(f"delete_{name}", crud["delete"])
        mutation_methods[f"delete_{name}"] = delete_fn

    # Create Query and Mutation dataclasses dynamically
    Query = type("Query", (), query_methods)
    Mutation = type("Mutation", (), mutation_methods)

    return Query, Mutation


def _make_crud_method(name, crud_info):
    """Create a classmethod with __puff_sql__ attributes."""
    def method(cls, context, /, **kwargs):
        pass  # Never called -- @sql bypasses Python

    method.__name__ = name
    method.__puff_sql__ = crud_info["sql"]
    method.__puff_sql_args__ = crud_info["args"]
    method = classmethod(method)
    return method
