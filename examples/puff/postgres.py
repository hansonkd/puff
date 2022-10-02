from puff import wrap_async, rust_objects

threadsafety = 3
apilevel = "2.0"
paramstyle = "qmark"


class PostgresCursor:
    def __init__(self, cursor):
        self.cursor = cursor

    def execute(self, q, params=None):
        return wrap_async(lambda r: self.cursor.execute(r, q, params), join=True)

    def executemany(self, q, seq_of_params=None):
        return wrap_async(lambda r: self.cursor.executemany(r, q, seq_of_params), join=True)

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


class PostgresConnection:
    def __init__(self, client=None):
        self.postgres_client = client or rust_objects.global_postgres_getter()

    def cursor(self) -> PostgresCursor:
        return PostgresCursor(self.postgres_client.cursor())

    def close(self):
        self.postgres_client.close()

    def commit(self):
        wrap_async(lambda r: self.postgres_client.commit(r), join=True)

    def rollback(self):
        wrap_async(lambda r: self.postgres_client.rollback(r), join=True)


