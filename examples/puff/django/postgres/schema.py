import logging

from django.db.backends.postgresql.schema import (
    DatabaseSchemaEditor as PGDatabaseSchemaEditor,
)


class DatabaseSchemaEditor(PGDatabaseSchemaEditor):
    pass
