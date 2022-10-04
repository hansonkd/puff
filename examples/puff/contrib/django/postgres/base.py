# import puff.postgres as Database

from .client import DatabaseClient  # NOQA
from .creation import DatabaseCreation  # NOQA
from .features import DatabaseFeatures  # NOQA
from .introspection import DatabaseIntrospection  # NOQA
from .operations import DatabaseOperations  # NOQA
from .schema import DatabaseSchemaEditor  # NOQA


from django.db.backends.postgresql.base import (
    DatabaseWrapper as PGDatabaseWrapper,
)


class DatabaseWrapper(PGDatabaseWrapper):
    # Database = Database
    display_name = "PuffPostgres"
    SchemaEditorClass = DatabaseSchemaEditor
    creation_class = DatabaseCreation
    features_class = DatabaseFeatures
    introspection_class = DatabaseIntrospection
    ops_class = DatabaseOperations
    client_class = DatabaseClient

    def _set_autocommit(self, autocommit):
        with self.wrap_database_errors:
            self.connection.set_autocommit(autocommit)

    def ensure_timezone(self):
        if self.connection is None:
            return False
        conn_timezone_name = self.connection.get_parameter_status("TimeZone")
        timezone_name = self.timezone_name
        if timezone_name and conn_timezone_name != timezone_name:
            with self.connection.cursor() as cursor:
                cursor.execute(self.ops.set_time_zone_sql() % timezone_name, [])
            return True
        return False
