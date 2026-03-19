"""Simple database migration system for Puff.

Tracks applied migrations in a ``puff_migrations`` table.
Migrations are plain SQL files stored in a ``migrations/`` directory
and are applied in alphabetical order.

Usage:
    from puff.migrations import migrate, generate_migration

    # Apply all pending migrations
    migrate()

    # Generate a new migration file
    generate_migration("add_email_column",
                       "ALTER TABLE users ADD COLUMN email TEXT")

    # Generate CREATE TABLE migrations from @model classes
    from puff.migrations import generate_model_migration
    generate_model_migration(Customer, Order)
"""
import os
from puff.postgres import connect


MIGRATIONS_TABLE = """
CREATE TABLE IF NOT EXISTS puff_migrations (
    id SERIAL PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    applied_at TIMESTAMPTZ DEFAULT NOW()
)
"""


def get_connection():
    """Return a new Postgres connection from the Puff pool."""
    return connect()


def ensure_migrations_table(conn):
    """Create the migrations tracking table if it does not exist."""
    with conn.cursor() as cur:
        cur.execute(MIGRATIONS_TABLE)
    conn.commit()


def get_applied_migrations(conn):
    """Return a list of migration names that have already been applied."""
    with conn.cursor() as cur:
        cur.execute("SELECT name FROM puff_migrations ORDER BY id")
        return [row[0] for row in cur.fetchall()]


def apply_migration(conn, name, sql):
    """Apply a single migration inside a transaction."""
    with conn.cursor() as cur:
        cur.execute(sql)
        cur.execute(
            "INSERT INTO puff_migrations (name) VALUES (%s)", [name]
        )
    conn.commit()


def migrate(migrations_dir="migrations"):
    """Apply all pending migrations from the migrations directory.

    Migration files are named like ``001_create_users.sql``,
    ``002_add_email.sql``, etc.  They are applied in alphabetical order.
    """
    conn = get_connection()
    ensure_migrations_table(conn)
    applied = set(get_applied_migrations(conn))

    if not os.path.exists(migrations_dir):
        os.makedirs(migrations_dir)
        print(f"Created {migrations_dir}/ directory")
        return

    pending = []
    for filename in sorted(os.listdir(migrations_dir)):
        if filename.endswith(".sql") and filename not in applied:
            pending.append(filename)

    if not pending:
        print("No pending migrations.")
        return

    for filename in pending:
        filepath = os.path.join(migrations_dir, filename)
        with open(filepath) as f:
            sql = f.read()
        print(f"Applying {filename}...")
        try:
            apply_migration(conn, filename, sql)
            print(f"  OK: {filename}")
        except Exception as e:
            print(f"  FAIL: {filename}: {e}")
            conn.rollback()
            raise

    print(f"Applied {len(pending)} migration(s).")
    conn.close()


def generate_migration(name, sql, migrations_dir="migrations"):
    """Generate a new numbered migration file.

    Returns the path to the created file.
    """
    if not os.path.exists(migrations_dir):
        os.makedirs(migrations_dir)

    existing = sorted(
        f for f in os.listdir(migrations_dir) if f.endswith(".sql")
    )
    next_num = len(existing) + 1
    filename = f"{next_num:03d}_{name}.sql"
    filepath = os.path.join(migrations_dir, filename)

    with open(filepath, "w") as f:
        f.write(sql + "\n")

    print(f"Created {filepath}")
    return filepath


def generate_model_migration(*models, migrations_dir="migrations"):
    """Generate CREATE TABLE migrations from @model decorated classes."""
    statements = []
    for model_cls in models:
        if hasattr(model_cls, "__puff_create_table__"):
            statements.append(model_cls.__puff_create_table__)

    if statements:
        sql = ";\n".join(statements)
        name = "create_models"
        return generate_migration(name, sql, migrations_dir)
