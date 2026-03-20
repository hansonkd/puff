"""Python tool decorator and metadata helpers for agent skills."""

import inspect
import typing


def _schema_for_annotation(annotation):
    if annotation is inspect._empty:
        return {"type": "string"}

    origin = typing.get_origin(annotation)
    args = typing.get_args(annotation)

    if annotation is str:
        return {"type": "string"}
    if annotation is int:
        return {"type": "integer"}
    if annotation is float:
        return {"type": "number"}
    if annotation is bool:
        return {"type": "boolean"}
    if annotation in (dict, typing.Dict):
        return {"type": "object"}
    if annotation in (list, tuple, set, typing.List, typing.Tuple, typing.Set):
        return {"type": "array"}

    if origin in (list, tuple, set, typing.List, typing.Tuple, typing.Set):
        item_schema = (
            _schema_for_annotation(args[0])
            if args
            else {"type": "string"}
        )
        return {"type": "array", "items": item_schema}

    if origin in (dict, typing.Dict):
        return {"type": "object"}

    if origin is typing.Union and args:
        non_none = [arg for arg in args if arg is not type(None)]
        if len(non_none) == 1:
            return _schema_for_annotation(non_none[0])

    return {"type": "string"}


def _build_input_schema(func):
    signature = inspect.signature(func)
    properties = {}
    required = []

    for name, param in signature.parameters.items():
        if param.kind not in (
            inspect.Parameter.POSITIONAL_OR_KEYWORD,
            inspect.Parameter.KEYWORD_ONLY,
        ):
            continue

        properties[name] = _schema_for_annotation(param.annotation)
        if param.default is inspect._empty:
            required.append(name)

    return {
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": False,
    }


def _tool_metadata(
    func,
    *,
    name=None,
    description=None,
    requires_approval=False,
    timeout_ms=30000,
):
    doc = inspect.getdoc(func) or ""
    summary = description or (doc.splitlines()[0].strip() if doc else "")
    return {
        "name": name or func.__name__,
        "description": summary,
        "input_schema": _build_input_schema(func),
        "requires_approval": bool(requires_approval),
        "timeout_ms": int(timeout_ms),
    }


def tool(
    func=None,
    *,
    name=None,
    description=None,
    requires_approval=False,
    timeout_ms=30000,
):
    """Mark a Python function as an agent tool.

    The decorator attaches JSON-serializable metadata that the Rust runtime
    reads when loading `tools_module` from agent config or skill config.
    """

    def decorate(inner):
        inner.__puff_tool__ = _tool_metadata(
            inner,
            name=name,
            description=description,
            requires_approval=requires_approval,
            timeout_ms=timeout_ms,
        )
        return inner

    if func is None:
        return decorate
    return decorate(func)
