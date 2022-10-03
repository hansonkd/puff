__version__ = "2.9.9"

import puff.postgres

READ_UNCOMMITTED = object()
READ_COMMITTED = object()
REPEATABLE_READ = object()
SERIALIZABLE = object()
DEFAULT = object()


def connect(*args, **kwargs):
    return puff.postgres.connect()
