import collections.abc
import functools
import inspect
from dataclasses import dataclass, fields, is_dataclass
from functools import wraps
from typing import (
    Any,
    Optional,
    Dict,
    Callable,
    Union,
    get_origin,
    get_args,
    List,
    get_type_hints,
    Tuple,
    ForwardRef,
    Iterable,
    Iterator,
)

from . import wrap_async, rust_objects
from .postgres import set_connection_override, PostgresConnection


@dataclass
class EmptyObject:
    pass


class Apps:
    ready = False


try:
    from django.apps import apps
    from django.db import connection as django_connection
except ImportError:
    django_connection = None
    apps = Apps()


def nested_dataclass(*args, **kwargs):
    def wrapper(cls):
        cls = dataclass(cls, **kwargs)
        original_init = cls.__init__
        cls.__init__ = wrap_method(original_init, cls.__annotations__, False)
        return cls

    return wrapper(args[0]) if args else wrapper


def parent(parent_field):
    def decorator(func):
        @wraps(func)
        def wrapper(*args, **kwargs):
            return func(*args, **kwargs)

        wrapper.parent_field = parent_field
        return wrapper

    return decorator


def needs_puff(func):
    func.safe_without_context = False
    return func


def pure(func):
    func.safe_without_context = True
    return func


def borrow_db_context(func):
    func.safe_without_context = False
    return func


NoneType = type(None)


@dataclass
class ParameterDescription:
    param_type: Any
    default: Any


@dataclass
class FieldDescription:
    return_type: Any
    arguments: Dict[str, Any]
    is_async: bool = False
    safe_without_context: bool = False
    producer: Optional[Callable[[Any], Any]] = None
    acceptor: Optional[Callable[[Callable[[Any], None]], None]] = None
    depends_on: Optional[List[str]] = None
    value_from_column: Optional[str] = None
    default: Any = None


@dataclass
class TypeDescription:
    type_info: str
    optional: bool
    inner_type: Optional["TypeDescription"] = None


def type_to_scalar(
    t, all_types, input_types, is_input, optional=False
) -> TypeDescription:
    origin = get_origin(t)

    if origin == Optional:
        optional = True
        t = get_args(t)[0]
        return type_to_scalar(t, all_types, input_types, is_input, optional)
    elif origin == Union and get_args(t)[1] is NoneType:
        optional = True
        t = get_args(t)[0]
        return type_to_scalar(t, all_types, input_types, is_input, optional)

    if origin == list or origin == List:
        return TypeDescription(
            optional=optional,
            type_info="List",
            inner_type=type_to_scalar(get_args(t)[0], all_types, input_types, is_input),
        )
    if t == str:
        return TypeDescription(optional=optional, type_info="String")
    elif t == int:
        return TypeDescription(optional=optional, type_info="Int")
    elif t == float:
        return TypeDescription(optional=optional, type_info="Float")
    elif t == bool:
        return TypeDescription(optional=optional, type_info="Boolean")
    elif t == Any:
        return TypeDescription(optional=optional, type_info="Any")
    elif isinstance(t, str):
        return TypeDescription(optional=optional, type_info=t)
    elif isinstance(t, ForwardRef):
        type_for_forward_ref = str(t)[12:-2]
        return TypeDescription(optional=optional, type_info=type_for_forward_ref)
    elif is_dataclass(t):
        load_aggro_type(t, all_types, input_types, is_input)
        type_name = get_type_name(t)
        return TypeDescription(optional=optional, type_info=type_name)

    raise Exception("Invalid type: {}".format(t))


PARENTS_VAR = "parents"
CONTEXT_VAR = "context"


def get_type_name(t):
    if isinstance(t, str):
        return t
    try:
        return t.__typename__
    except AttributeError:
        return t.__name__


def expand_typehints(type_hints):
    expanded_hints = {}
    for arg_name, arg_field_type in type_hints.items():
        origin = get_origin(arg_field_type)
        if origin == Optional:
            arg_field_type = get_args(arg_field_type)[0]
        elif origin == Union and get_args(arg_field_type)[1] is NoneType:
            arg_field_type = get_args(arg_field_type)[0]
        origin = get_origin(arg_field_type)
        is_list = False
        if origin == list or origin == List:
            is_list = True
            inner = get_args(arg_field_type)[0]
            inner_origin = get_origin(inner)
            arg_field_type = inner
            if inner_origin == Optional:
                arg_field_type = get_args(inner_origin)[0]
            elif inner_origin == Union and get_args(inner_origin)[1] is NoneType:
                arg_field_type = get_args(inner_origin)[0]

        expanded_hints[arg_name] = (is_list, arg_field_type)
    return expanded_hints


def wrap_method(method, type_hints, is_async=False):
    expanded_hints = expand_typehints(type_hints)

    if is_async:

        @wraps(method)
        async def wrapped_method(*args, **kwargs):
            if len(args) == 1:
                ctx = args[0]
                new_args = (GraphQLContext(ctx),)
            else:
                ctx = args[1]
                new_args = (args[0], GraphQLContext(ctx))

            if apps.ready and django_connection is not None:
                django_connection.connection = None
            set_connection_override(ctx.connection())
            for arg_name, arg_value in kwargs.items():
                is_list, arg_field_type = expanded_hints.get(arg_name, None)

                if (
                    is_list
                    and is_dataclass(arg_field_type)
                    and isinstance(arg_value, list)
                ):
                    new_obj = [
                        arg_field_type(**v) if isinstance(v, dict) else v
                        for v in arg_value
                    ]
                    kwargs[arg_name] = new_obj
                elif is_dataclass(arg_field_type) and isinstance(arg_value, dict):
                    new_obj = arg_field_type(**arg_value)
                    kwargs[arg_name] = new_obj
            r = await method(*new_args, **kwargs)
            set_connection_override(None)
            if apps.ready and django_connection is not None:
                django_connection.connection = None
            return r

    else:

        @wraps(method)
        def wrapped_method(*args, **kwargs):
            if len(args) == 1:
                ctx = args[0]
                new_args = (GraphQLContext(ctx),)
            else:
                ctx = args[1]
                new_args = (args[0], GraphQLContext(ctx))

            if apps.ready and django_connection is not None:
                django_connection.connection = None
            set_connection_override(ctx.connection())
            for arg_name, arg_value in kwargs.items():
                is_list, arg_field_type = expanded_hints.get(arg_name, None)

                if (
                    is_list
                    and is_dataclass(arg_field_type)
                    and isinstance(arg_value, list)
                ):
                    new_obj = [
                        arg_field_type(**v) if isinstance(v, dict) else v
                        for v in arg_value
                    ]
                    kwargs[arg_name] = new_obj
                elif is_dataclass(arg_field_type) and isinstance(arg_value, dict):
                    new_obj = arg_field_type(**arg_value)
                    kwargs[arg_name] = new_obj
            r = method(*new_args, **kwargs)
            set_connection_override(None)
            if apps.ready and django_connection is not None:
                django_connection.connection = None
            return r

    return wrapped_method


def load_aggro_type(t, all_types, input_types, is_input, name=None):
    type_name = name or get_type_name(t)

    properties = {}
    if is_input:
        if type_name in all_types:
            raise Exception(
                f"Tried registering input type {type_name} when it already exists as a normal type"
            )
        if type_name in input_types:
            return
        input_types[type_name] = properties
    else:
        if type_name in input_types:
            raise Exception(
                f"Tried registering type {type_name} when it already exists as an input type"
            )
        if type_name in all_types:
            return
        all_types[type_name] = properties

    for field in fields(t):
        field_t = field.type

        field_type = type_to_scalar(field_t, all_types, input_types, is_input)
        db_column = field.name
        desc = FieldDescription(
            return_type=field_type,
            arguments={},
            depends_on=[db_column],
            safe_without_context=True,
            value_from_column=db_column,
            default=field.default,
        )
        properties[field.name] = desc

    if is_input:
        return

    method_list = inspect.getmembers(t, predicate=inspect.ismethod)
    for (method_name, _method) in method_list:
        if method_name.startswith("_"):
            continue
        method = getattr(t, method_name)
        type_hints = get_type_hints(method)
        signature = inspect.signature(method)
        params = signature.parameters
        if method.__self__ is t:
            arguments = {}
            positional_only = []
            for param_name, param in params.items():
                default = None
                if param.default != inspect.Parameter.empty:
                    default = param.default
                if param.kind == inspect.Parameter.POSITIONAL_ONLY:
                    positional_only.append(param_name)
                    continue
                if param.kind == inspect.Parameter.POSITIONAL_OR_KEYWORD:
                    annotation = param.annotation
                    if annotation == inspect.Parameter.empty:
                        raise Exception(
                            f"Keyword argument for field {param_name} in {method_name} in type {t.__name__} does not have annotation"
                        )
                    param_t = annotation
                    scalar_t = type_to_scalar(param_t, all_types, input_types, True)
                    arguments[param_name] = ParameterDescription(
                        param_type=scalar_t, default=default
                    )
                else:
                    raise Exception(
                        f"Invalid Parameter {param_name} in {method_name} in type {t.__name__}"
                    )

            if len(positional_only) != 1:
                raise Exception(
                    f"Graphql field expected exactly 1 positional only context arg, instead got {positional_only}"
                )

            if signature.return_annotation == inspect.Signature.empty:
                raise Exception(f"Return typ for Graphql field {method_name} is empty")

            return_t = signature.return_annotation
            origin = get_origin(return_t)
            acceptor = None
            is_async = inspect.iscoroutinefunction(method)
            wrapped_method = wrap_method(method, type_hints, is_async=is_async)
            if origin in (
                Iterator,
                Iterable,
                collections.abc.Iterable,
                collections.abc.Iterator,
            ):
                iterable_args = get_args(return_t)
                return_t = iterable_args[0]
                origin = get_origin(return_t)
                acceptor = make_acceptor(wrapped_method, is_async)

            if origin in (Tuple, tuple):
                tuple_args = get_args(return_t)
                tuple_arg_len = len(tuple_args)
                return_t = tuple_args[0]
                if tuple_arg_len == 2:
                    pass
                elif tuple_arg_len in (3, 5):
                    if not (tuple_args[1] == str and get_origin(tuple_args[2]) == list):
                        raise Exception(
                            f"Expected the second argument of a tuple to be a string and the 3rd argument to be a list {method_name}: {tuple_args}"
                        )
                elif tuple_arg_len in (4,):
                    if not (tuple_args[1] == list):
                        raise Exception(
                            f"Expected the second argument of a tuple to be a list {method_name}: {tuple_args}"
                        )
                else:
                    raise Exception(
                        f"Invalid number of tuple arguments for return type of {method_name}: {tuple_args}"
                    )

            depends_on = getattr(method, "depends_on", None)
            safe_without_context = getattr(method, "safe_without_context", True)

            desc = FieldDescription(
                return_type=type_to_scalar(return_t, all_types, input_types, is_input),
                safe_without_context=safe_without_context,
                arguments=arguments,
                acceptor=acceptor,
                producer=wrapped_method,
                is_async=is_async,
                depends_on=depends_on,
            )
            properties[method_name] = desc


class GraphQLContext:
    def __init__(self, ctx):
        self.ctx = ctx

    @property
    def auth(self):
        return self.ctx.auth

    @property
    def connection(self):
        return self.ctx.connection()

    def parent_values(self, parent_fields):
        return self.ctx.parent_values(parent_fields)


def make_acceptor(method, is_async):
    if is_async:

        async def acceptor(initiate, ctx, lookahead):
            async for result in method(ctx, **lookahead):

                def render_fn(*args, **kwargs):
                    return result

                if not await wrap_async(lambda r: initiate(r, render_fn)):
                    break

        return acceptor
    else:

        def acceptor(initiate, ctx, lookahead):
            for result in method(ctx, **lookahead):

                def render_fn(*args, **kwargs):
                    return result

                if not wrap_async(lambda r: initiate(r, render_fn)):
                    break

        return acceptor


Description = Dict[str, FieldDescription]
Descriptions = Dict[str, Description]


@dataclass
class AuthRejection:
    status_code: int = 403
    message: str = "Access Denied"
    headers: dict = None
    is_rejection: bool = True


@dataclass
class SchemaDescription:
    input_types: Descriptions
    all_types: Descriptions
    auth_function: Optional[Callable[[Any], Union[Any, AuthRejection]]] = None
    auth_async: bool = False


SchemaInput = Union[dataclass, SchemaDescription]


class PermissionDenied(Exception):
    def __init__(
        self,
        status=403,
        response_message="Permission denied",
        headers=None,
    ):
        self.status = status
        self.response_message = response_message
        self.headers = {} if headers is None else headers
        super().__init__()


def wrap_auth(auth, auth_async):
    if auth:
        if auth_async:

            @functools.wraps(auth)
            async def wrapped_auth(headers):
                try:
                    return await auth(headers)
                except PermissionDenied as e:
                    return AuthRejection(
                        status_code=e.status,
                        message=e.response_message,
                        headers=e.headers,
                    )

            return wrapped_auth
        else:

            @functools.wraps(auth)
            def wrapped_auth(headers):
                try:
                    return auth(headers)
                except PermissionDenied as e:
                    return AuthRejection(
                        status_code=e.status,
                        message=e.response_message,
                        headers=e.headers,
                    )

            return wrapped_auth
    return auth


def type_to_description(schema: SchemaInput) -> SchemaDescription:
    if isinstance(schema, SchemaDescription):
        return schema

    schema_fields = {f.name: f.type for f in fields(schema)}
    all_types = {}
    input_types = {}
    query = schema_fields["query"]
    mutation = schema_fields.get("mutation", None)
    subscription = schema_fields.get("subscription", None)
    auth = getattr(schema, "auth", None)
    auth_async = False
    if auth and inspect.iscoroutinefunction(auth):
        auth_async = True

    load_aggro_type(query, all_types, input_types, False, name="Query")
    if mutation:
        load_aggro_type(mutation, all_types, input_types, False, name="Mutation")
    if subscription:
        load_aggro_type(
            subscription, all_types, input_types, False, name="Subscription"
        )
    return SchemaDescription(
        all_types=all_types,
        input_types=input_types,
        auth_function=wrap_auth(auth, auth_async),
        auth_async=auth_async,
    )


class GraphqlSubscription:
    def __init__(self, rec):
        self.rec = rec

    def receive(self) -> Optional[Tuple[Optional[str], dict]]:
        return wrap_async(
            lambda rr: self.rec(rr),
        )


class GraphqlClient:
    def __init__(self, client=None, client_fn=None):
        self.gql = client
        self.client_fn = client_fn or rust_objects.global_gql_getter

    def client(self):
        gql = self.gql
        if gql is None:
            self.gql = gql = self.client_fn()
        return gql

    def query(
        self,
        query: str,
        variables: Dict[str, Any],
        connection: PostgresConnection = None,
        auth_token: str = None,
    ) -> Any:
        conn = connection and connection.postgres_client

        return wrap_async(
            lambda rr: self.client().query(rr, query, variables, conn, auth_token),
            join=True,
        )

    def subscribe(
        self,
        query: str,
        variables: Dict[str, Any],
        connection: PostgresConnection = None,
        auth_token: str = None,
    ) -> GraphqlSubscription:
        conn = connection and connection.postgres_client

        return wrap_async(
            lambda rr: self.client().subscribe(rr, query, variables, conn, auth_token),
            wrap_return=GraphqlSubscription,
        )


global_graphql = GraphqlClient()


def named_client(name: str = "default"):
    return GraphqlClient(client_fn=lambda: rust_objects.global_gql_getter.by_name(name))
