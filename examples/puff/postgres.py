import contextvars

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


class PostgresCursor:
    def __init__(self, cursor, connection):
        self.cursor = cursor
        self.last_query = None
        self.connection = connection
        self.current_transaction = False

    @property
    def rowcount(self):
        return wrap_async(lambda r: self.cursor.do_get_rowcount(r), join=True)

    def execute(self, q, params=None):
        self.last_query = q.encode("utf8")
        ix = 1
        params = list(params) if params is not None else None
        while "%s" in q:
            q = q.replace("%s", f"${ix}", 1)
            ix += 1
        ret = wrap_async(lambda r: self.cursor.execute(r, q, params), join=True)
        self.current_transaction = True
        return ret

    def executemany(self, q, seq_of_params=None):
        for params in seq_of_params:
            self.execute(q, params)

    def fetchone(self):
        return wrap_async(lambda r: self.cursor.fetchone(r), join=True)

    def fetchmany(self, rowcount=None):
        return wrap_async(lambda r: self.cursor.fetchmany(r, rowcount), join=True)

    def fetchall(self):
        return wrap_async(lambda r: self.cursor.fetchall(r), join=True)

    def close(self):
        return self.cursor.close()

    def __del__(self):
        return self.close()

    def __iter__(self):
        return self

    def __next__(self):
        return self.fetchone()

    def __enter__(self):
        return self

    def __exit__(self, *args, **kwargs):
        self.close()

    @property
    def query(self):
        return self.last_query


class PostgresConnection:
    isolation_level = ISOLATION_LEVEL_DEFAULT
    server_version = 140000

    def __init__(self, client=None, autocommit=False):
        self._autocommit = autocommit
        self.postgres_client = client or rust_objects.global_postgres_getter()

    def __enter__(self):
        return self

    def __exit__(self):
        self.close()

    @property
    def autocommit(self):
        return self._autocommit

    @autocommit.setter
    def autocommit(self, value):
        self._autocommit = value
        self.postgres_client.set_auto_commit(value)

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
        return wrap_async(lambda r: self.postgres_client.commit(r), join=True)

    def rollback(self):
        return wrap_async(lambda r: self.postgres_client.rollback(r), join=True)


def connect(*parameters, **kwargs) -> PostgresConnection:
    real_kwargs = {}
    valid_params = ["autocommit"]
    for param in valid_params:
        if param in kwargs:
            real_kwargs[param] = kwargs[param]
    conn = PostgresConnection(**real_kwargs)
    return conn
