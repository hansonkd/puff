"""Automatic audit logging for Puff GraphQL mutations.

Records who changed what, when, and the mutation details in a
``puff_audit_log`` Postgres table.

Usage:
    from puff.audit import AuditMiddleware, log_mutation, get_audit_log

    # Wrap a schema for automatic mutation auditing
    schema = AuditMiddleware(original_schema)

    # Manual logging
    log_mutation(mutation="createUser", variables={"name": "Alice"})

    # Query the audit log
    rows = get_audit_log(limit=50, agent="assistant")
"""
import json
from puff.postgres import connect


AUDIT_TABLE = """
CREATE TABLE IF NOT EXISTS puff_audit_log (
    id SERIAL PRIMARY KEY,
    timestamp TIMESTAMPTZ DEFAULT NOW(),
    agent TEXT,
    mutation TEXT NOT NULL,
    variables JSONB,
    result JSONB,
    user_id TEXT,
    ip_address TEXT
)
"""


def ensure_audit_table():
    """Create the audit log table if it does not exist."""
    conn = connect()
    with conn.cursor() as cur:
        cur.execute(AUDIT_TABLE)
    conn.commit()
    conn.close()


def log_mutation(
    mutation,
    variables=None,
    result=None,
    agent=None,
    user_id=None,
    ip_address=None,
):
    """Log a mutation to the audit trail."""
    conn = connect()
    with conn.cursor() as cur:
        cur.execute(
            "INSERT INTO puff_audit_log "
            "(agent, mutation, variables, result, user_id, ip_address) "
            "VALUES (%s, %s, %s, %s, %s, %s)",
            [
                agent,
                mutation,
                json.dumps(variables) if variables else None,
                json.dumps(result) if result else None,
                user_id,
                ip_address,
            ],
        )
    conn.commit()
    conn.close()


def get_audit_log(limit=100, agent=None, mutation=None):
    """Query the audit log with optional filters.

    Args:
        limit: Maximum number of rows to return.
        agent: Filter by agent name (exact match).
        mutation: Filter by mutation text (substring match via LIKE).

    Returns:
        A list of tuples (id, timestamp, agent, mutation, variables,
        result, user_id).
    """
    conn = connect()
    with conn.cursor() as cur:
        conditions = []
        params = []
        if agent:
            conditions.append("agent = %s")
            params.append(agent)
        if mutation:
            conditions.append("mutation LIKE %s")
            params.append(f"%{mutation}%")

        where = f" WHERE {' AND '.join(conditions)}" if conditions else ""
        params.append(limit)
        cur.execute(
            f"SELECT id, timestamp, agent, mutation, variables, result, user_id "
            f"FROM puff_audit_log{where} ORDER BY id DESC LIMIT %s",
            params,
        )
        rows = cur.fetchall()
    conn.close()
    return rows


class AuditMiddleware:
    """Wraps a GraphQL schema to automatically audit all mutations.

    Usage:
        from puff.audit import AuditMiddleware

        schema = AuditMiddleware(original_schema)
        result = schema.query("mutation { createUser(name: \\"Alice\\") { id } }")
    """

    def __init__(self, schema):
        self.schema = schema
        ensure_audit_table()

    def query(self, query_str, variables=None, **kwargs):
        """Execute a query, auditing mutations automatically."""
        result = self.schema.query(query_str, variables, **kwargs)

        # Auto-audit mutations
        if query_str.strip().lower().startswith("mutation"):
            log_mutation(
                mutation=query_str,
                variables=variables,
                result=result,
                agent=kwargs.get("agent"),
            )

        return result
