"""
Puff Postgres — DB-API 2.0 compatible database adapter.

The Rust cursor returns (rows, description, rowcount) from execute().
Fetch methods read from the stored rows — no Rust callbacks needed.
Parameter placeholders (%s) are converted to $N in Rust, not here.
"""
import contextvars
import datetime
import functools
import time
import sys

from puff import wrap_async, rust_objects


threadsafety = 3
apilevel = "2.0"
paramstyle = "format"

"""Isolation level values."""
ISOLATION_LEVEL_AUTOCOMMIT = 0
ISOLATION_LEVEL_READ_UNCOMMITTED = 4
ISOLATION_LEVEL_READ_COMMITTED = 1
ISOLATION_LEVEL_REPEATABLE_READ = 2
ISOLATION_LEVEL_SERIALIZABLE = 3
ISOLATION_LEVEL_DEFAULT = None


@functools.total_ordering
class TypeObject(object):
    def __init__(self, *value_names):
        self.value_names = value_names
        self.values = value_names

    def __eq__(self, other):
        return other in self.values

    def __lt__(self, other):
        return self != other and other < self.values

    def __repr__(self):
        return "TypeObject" + str(self.value_names)


def _binary(string):
    if isinstance(string, str):
        return string.encode("utf-8")
    return bytes(string)


STRING = TypeObject("TEXT", "VARCHAR")
BINARY = TypeObject("BYTEA")
NUMBER = TypeObject("INT2", "INT4", "INT8", "FLOAT4", "FLOAT8")
DATETIME = TypeObject("TIMESTAMPZ", "TIMESTAMP")
DATE = TypeObject("DATE")
TIME = TypeObject("TIME")
ROWID = STRING

Datetime = datetime.datetime
Binary = _binary
Timestamp = Datetime
Date = datetime.date
Time = datetime.time

DatetimeFromTicks = Datetime.fromtimestamp
TimestampFromTicks = Timestamp.fromtimestamp
DateFromTicks = Date.fromtimestamp


def TimeFromTicks(ticks):
    return time.gmtime(round(ticks))


class PostgresCursor:
    """DB-API 2.0 Cursor.

    The Rust cursor returns (rows, description, rowcount) from execute().
    Fetch methods read from the locally stored rows — no Rust round-trips.
    """

    def __init__(self, cursor, connection):
        self.cursor = cursor
        self.last_query = None
        self.connection = connection
        self._rows = []
        self._row_index = 0
        self._description = None
        self._rowcount = -1

    @property
    def rowcount(self):
        return self._rowcount

    @property
    def description(self):
        return self._description

    @property
    def arraysize(self):
        return self.cursor.arraysize

    @arraysize.setter
    def arraysize(self, val):
        self.cursor.arraysize = val

    def setinputsizes(self, *args, **kwargs):
        pass

    def setoutputsize(self, *args, **kwargs):
        pass

    def execute(self, q, params=None):
        """Execute a query. Rows are fetched eagerly and stored locally."""
        self.last_query = q.encode("utf8") if isinstance(q, str) else q
        self._rows = []
        self._row_index = 0
        self._description = None
        self._rowcount = -1

        # Don't convert %s here — Rust does it via convert_placeholders()
        params = list(params) if params is not None else None
        result = wrap_async(lambda r: self.cursor.execute(r, q, params))

        # Rust returns (rows_list, description, rowcount)
        if result is not None and isinstance(result, tuple) and len(result) == 3:
            self._rows = result[0] if result[0] is not None else []
            self._description = result[1]
            self._rowcount = result[2]
        return self._rowcount

    def executemany(self, q, seq_of_params=None):
        """Execute a query against multiple parameter sets."""
        self._rows = []
        self._row_index = 0
        self._description = None
        self._rowcount = -1

        result = wrap_async(lambda r: self.cursor.executemany(r, q, seq_of_params))
        if result is not None and isinstance(result, tuple) and len(result) == 3:
            self._rows = result[0] if result[0] is not None else []
            self._description = result[1]
            self._rowcount = result[2]
        return self._rowcount

    def fetchone(self):
        """Return the next row, or None."""
        if self._row_index >= len(self._rows):
            return None
        row = self._rows[self._row_index]
        self._row_index += 1
        return row

    def fetchmany(self, size=None):
        """Return up to `size` rows."""
        if size is None:
            size = self.arraysize
        end = min(self._row_index + size, len(self._rows))
        rows = self._rows[self._row_index:end]
        self._row_index = end
        return rows

    def fetchall(self):
        """Return all remaining rows."""
        rows = self._rows[self._row_index:]
        self._row_index = len(self._rows)
        return rows

    def close(self):
        return self.cursor.close()

    def __del__(self):
        try:
            self.close()
        except Exception:
            pass

    def __iter__(self):
        return self

    def __next__(self):
        row = self.fetchone()
        if row is None:
            raise StopIteration
        return row

    def __enter__(self):
        return self

    def __exit__(self, *args, **kwargs):
        self.close()

    @property
    def query(self):
        return self.last_query

    def callproc(self, procname, parameters=None):
        pass


def get_client(dbname):
    if dbname is None:
        return rust_objects.global_postgres_getter()
    return rust_objects.global_postgres_getter.by_name(dbname)


class PostgresConnection:
    """DB-API 2.0 Connection."""

    isolation_level = ISOLATION_LEVEL_DEFAULT
    server_version = 140000

    def __init__(self, client=None, autocommit=False, dbname=None):
        self._autocommit = autocommit
        self.postgres_client = client or get_client(dbname)

    def __enter__(self):
        return self

    def __exit__(self, *args, **kwargs):
        self.close()

    @property
    def Warning(self):
        return sys.modules[__name__].Warning

    @property
    def Error(self):
        return sys.modules[__name__].Error

    @property
    def InterfaceError(self):
        return sys.modules[__name__].InterfaceError

    @property
    def DatabaseError(self):
        return sys.modules[__name__].DatabaseError

    @property
    def OperationalError(self):
        return sys.modules[__name__].OperationalError

    @property
    def IntegrityError(self):
        return sys.modules[__name__].IntegrityError

    @property
    def InternalError(self):
        return sys.modules[__name__].InternalError

    @property
    def ProgrammingError(self):
        return sys.modules[__name__].ProgrammingError

    @property
    def NotSupportedError(self):
        return sys.modules[__name__].NotSupportedError

    @property
    def autocommit(self):
        return self._autocommit

    @autocommit.setter
    def autocommit(self, value):
        self._autocommit = value
        wrap_async(lambda rr: self.postgres_client.set_auto_commit(rr, value))

    def set_client_encoding(self, encoding, *args, **kwargs):
        if encoding != "UTF8":
            raise Exception("Only UTF8 Postgres encoding supported.")

    def get_parameter_status(self, parameter, *args, **kwargs):
        with self.cursor() as cursor:
            cursor.execute("SELECT current_setting($1)", [parameter])
            return cursor.fetchone()

    def set_autocommit(self, autocommit):
        self.autocommit = autocommit

    def cursor(self, *args, **kwargs) -> PostgresCursor:
        return PostgresCursor(self.postgres_client.cursor(), self)

    def close(self):
        self.postgres_client.close()

    def commit(self):
        return wrap_async(lambda r: self.postgres_client.commit(r))

    def rollback(self):
        return wrap_async(lambda r: self.postgres_client.rollback(r))


connection_override = contextvars.ContextVar("connection_override")


def set_connection_override(connection):
    connection_override.set(connection)


def connect(*parameters, **kwargs) -> PostgresConnection:
    this_connection_override = connection_override.get(None)
    real_kwargs = {}
    if this_connection_override is not None:
        real_kwargs["client"] = this_connection_override

    valid_params = ["autocommit", "dbname"]
    for param in valid_params:
        if param in kwargs:
            real_kwargs[param] = kwargs[param]
    conn = PostgresConnection(**real_kwargs)
    return conn
