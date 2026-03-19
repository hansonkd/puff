use crate::context::with_puff_context;
use crate::errors::PuffResult;
use crate::python::async_python::handle_python_return;
use crate::python::get_cached_object;

use anyhow::anyhow;
use bb8_postgres::bb8::Pool;

use bb8_postgres::PostgresConnectionManager;
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use futures_util::future::try_join_all;
use futures_util::TryStreamExt;
use pyo3::create_exception;

use pyo3::prelude::*;
use pyo3::types::{PyList, PyString, PyTuple};
use pythonize::depythonize;
use std::collections::HashMap;
use std::error::Error;
use std::fmt::{Debug, Formatter};

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc::{self, Sender};
use tokio::sync::{oneshot, Mutex};
use tokio::runtime::Handle;
use tokio_postgres::error::SqlState;
use tokio_postgres::types::private::BytesMut;
use tokio_postgres::types::{to_sql_checked, IsNull, ToSql, Type};
use tokio_postgres::{Column, NoTls, Row, Statement};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Exception hierarchy (DB-API 2.0)
// ---------------------------------------------------------------------------

create_exception!(module, PgError, pyo3::exceptions::PyException);
create_exception!(module, Warning, pyo3::exceptions::PyException);
create_exception!(module, InterfaceError, PgError);
create_exception!(module, DatabaseError, PgError);
create_exception!(module, InternalError, DatabaseError);
create_exception!(module, OperationalError, DatabaseError);
create_exception!(module, ProgrammingError, DatabaseError);
create_exception!(module, IntegrityError, DatabaseError);
create_exception!(module, DataError, DatabaseError);
create_exception!(module, NotSupportedError, DatabaseError);

// ---------------------------------------------------------------------------
// DbOp — operations sent to the per-connection background task
// ---------------------------------------------------------------------------

enum DbOp {
    Query {
        sql: String,
        params: Vec<PythonSqlValue>,
        reply: oneshot::Sender<Result<QueryResult, PyErr>>,
    },
    ExecuteMany {
        sql: String,
        param_seq: Vec<Vec<PythonSqlValue>>,
        reply: oneshot::Sender<Result<QueryResult, PyErr>>,
    },
    /// ExecuteRust is used by the GraphQL engine — returns raw (Statement, Vec<Row>)
    ExecuteRust {
        sql: String,
        params: Vec<PythonSqlValue>,
        reply: oneshot::Sender<PuffResult<(Statement, Vec<Row>)>>,
    },
    /// ExecuteRustNative is used by the zero-Python fast path — pure Rust params
    ExecuteRustNative {
        sql: String,
        params: Vec<RustSqlValue>,
        reply: oneshot::Sender<PuffResult<(Statement, Vec<Row>)>>,
    },
    Commit {
        reply: oneshot::Sender<Result<(), PyErr>>,
    },
    Rollback {
        reply: oneshot::Sender<Result<(), PyErr>>,
    },
    SetAutoCommit {
        value: bool,
        reply: oneshot::Sender<Result<(), PyErr>>,
    },
    Close,
}

impl Debug for DbOp {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            DbOp::Query { .. } => f.write_str("Query"),
            DbOp::ExecuteMany { .. } => f.write_str("ExecuteMany"),
            DbOp::ExecuteRust { .. } => f.write_str("ExecuteRust"),
            DbOp::ExecuteRustNative { .. } => f.write_str("ExecuteRustNative"),
            DbOp::Commit { .. } => f.write_str("Commit"),
            DbOp::Rollback { .. } => f.write_str("Rollback"),
            DbOp::SetAutoCommit { .. } => f.write_str("SetAutoCommit"),
            DbOp::Close => f.write_str("Close"),
        }
    }
}

struct QueryResult {
    rows: Vec<Row>,
    statement: Statement,
    affected_rows: u64,
}

// ---------------------------------------------------------------------------
// Background connection task
// ---------------------------------------------------------------------------

async fn conn_task(pool: Pool<PostgresConnectionManager<NoTls>>, mut rx: mpsc::Receiver<DbOp>) {
    let client = match pool.get().await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Failed to acquire Postgres connection from pool: {e}");
            drain_on_error(&mut rx, &e).await;
            return;
        }
    };

    let mut stmt_cache: HashMap<String, Statement> = HashMap::new();
    let mut autocommit = false;
    let mut in_transaction = false;

    while let Some(op) = rx.recv().await {
        match op {
            DbOp::Query { sql, params, reply } => {
                let result = do_query(
                    &client,
                    &mut stmt_cache,
                    &sql,
                    &params,
                    autocommit,
                    &mut in_transaction,
                )
                .await;
                let _ = reply.send(result);
            }
            DbOp::ExecuteMany {
                sql,
                param_seq,
                reply,
            } => {
                let result = do_execute_many(
                    &client,
                    &mut stmt_cache,
                    &sql,
                    &param_seq,
                    autocommit,
                    &mut in_transaction,
                )
                .await;
                let _ = reply.send(result);
            }
            DbOp::ExecuteRust { sql, params, reply } => {
                let result = do_execute_rust(
                    &client,
                    &mut stmt_cache,
                    &sql,
                    &params,
                    autocommit,
                    &mut in_transaction,
                )
                .await;
                let _ = reply.send(result);
            }
            DbOp::ExecuteRustNative { sql, params, reply } => {
                let result = do_execute_rust_native(
                    &client,
                    &mut stmt_cache,
                    &sql,
                    &params,
                    autocommit,
                    &mut in_transaction,
                )
                .await;
                let _ = reply.send(result);
            }
            DbOp::Commit { reply } => {
                let result = if in_transaction {
                    match client.batch_execute("COMMIT").await {
                        Ok(()) => {
                            in_transaction = false;
                            Ok(())
                        }
                        Err(e) => {
                            in_transaction = false;
                            Err(postgres_to_python_exception(e))
                        }
                    }
                } else {
                    Ok(())
                };
                let _ = reply.send(result);
            }
            DbOp::Rollback { reply } => {
                let result = if in_transaction {
                    match client.batch_execute("ROLLBACK").await {
                        Ok(()) => {
                            in_transaction = false;
                            Ok(())
                        }
                        Err(e) => {
                            in_transaction = false;
                            Err(postgres_to_python_exception(e))
                        }
                    }
                } else {
                    Ok(())
                };
                let _ = reply.send(result);
            }
            DbOp::SetAutoCommit { value, reply } => {
                if value && in_transaction {
                    let _ = client.batch_execute("COMMIT").await;
                    in_transaction = false;
                }
                autocommit = value;
                let _ = reply.send(Ok(()));
            }
            DbOp::Close => {
                if in_transaction {
                    let _ = client.batch_execute("ROLLBACK").await;
                }
                break;
            }
        }
    }
}

/// Drain all pending ops with an error when the connection couldn't be acquired.
async fn drain_on_error(
    rx: &mut mpsc::Receiver<DbOp>,
    e: &bb8_postgres::bb8::RunError<tokio_postgres::Error>,
) {
    let msg = format!("Failed to acquire connection: {e}");
    while let Some(op) = rx.recv().await {
        match op {
            DbOp::Query { reply, .. } => {
                let _ = reply.send(Err(OperationalError::new_err(msg.clone())));
            }
            DbOp::ExecuteMany { reply, .. } => {
                let _ = reply.send(Err(OperationalError::new_err(msg.clone())));
            }
            DbOp::ExecuteRust { reply, .. } => {
                let _ = reply.send(Err(anyhow!(msg.clone())));
            }
            DbOp::ExecuteRustNative { reply, .. } => {
                let _ = reply.send(Err(anyhow!(msg.clone())));
            }
            DbOp::Commit { reply } => {
                let _ = reply.send(Err(OperationalError::new_err(msg.clone())));
            }
            DbOp::Rollback { reply } => {
                let _ = reply.send(Err(OperationalError::new_err(msg.clone())));
            }
            DbOp::SetAutoCommit { reply, .. } => {
                let _ = reply.send(Err(OperationalError::new_err(msg.clone())));
            }
            DbOp::Close => break,
        }
    }
}

async fn do_query(
    client: &tokio_postgres::Client,
    stmt_cache: &mut HashMap<String, Statement>,
    sql: &str,
    params: &[PythonSqlValue],
    autocommit: bool,
    in_transaction: &mut bool,
) -> Result<QueryResult, PyErr> {
    // If not autocommit and not yet in a transaction, issue BEGIN
    if !autocommit && !*in_transaction {
        client
            .batch_execute("BEGIN")
            .await
            .map_err(postgres_to_python_exception)?;
        *in_transaction = true;
    }

    let stmt = get_or_prepare(client, stmt_cache, sql).await?;

    if stmt.columns().is_empty() {
        // DML statement — use execute
        let affected = client
            .execute_raw(&stmt, params)
            .await
            .map_err(postgres_to_python_exception)?;
        Ok(QueryResult {
            rows: Vec::new(),
            statement: stmt,
            affected_rows: affected,
        })
    } else {
        // SELECT — fetch all rows
        let row_stream = client
            .query_raw(&stmt, params)
            .await
            .map_err(postgres_to_python_exception)?;
        let rows: Vec<Row> = row_stream
            .try_collect()
            .await
            .map_err(postgres_to_python_exception)?;
        Ok(QueryResult {
            rows,
            statement: stmt,
            affected_rows: 0,
        })
    }
}

async fn do_execute_many(
    client: &tokio_postgres::Client,
    stmt_cache: &mut HashMap<String, Statement>,
    sql: &str,
    param_seq: &[Vec<PythonSqlValue>],
    autocommit: bool,
    in_transaction: &mut bool,
) -> Result<QueryResult, PyErr> {
    if !autocommit && !*in_transaction {
        client
            .batch_execute("BEGIN")
            .await
            .map_err(postgres_to_python_exception)?;
        *in_transaction = true;
    }

    let stmt = get_or_prepare(client, stmt_cache, sql).await?;

    // Pipeline all executes concurrently on the same connection.
    // tokio-postgres supports pipelining: multiple queries are sent without
    // waiting for each response, and the Postgres wire protocol handles them
    // in order.
    let futures: Vec<_> = param_seq
        .iter()
        .map(|params| client.execute_raw(&stmt, &params[..]))
        .collect();
    let results = try_join_all(futures)
        .await
        .map_err(postgres_to_python_exception)?;
    let total_affected: u64 = results.iter().sum();

    Ok(QueryResult {
        rows: Vec::new(),
        statement: stmt,
        affected_rows: total_affected,
    })
}

async fn do_execute_rust(
    client: &tokio_postgres::Client,
    stmt_cache: &mut HashMap<String, Statement>,
    sql: &str,
    params: &[PythonSqlValue],
    autocommit: bool,
    in_transaction: &mut bool,
) -> PuffResult<(Statement, Vec<Row>)> {
    if !autocommit && !*in_transaction {
        client.batch_execute("BEGIN").await?;
        *in_transaction = true;
    }

    let stmt = match get_or_prepare(client, stmt_cache, sql).await {
        Ok(s) => s,
        Err(e) => return Err(anyhow!("{e}")),
    };

    let row_stream = client.query_raw(&stmt, params).await?;
    let rows: Vec<Row> = row_stream.try_collect().await?;
    Ok((stmt, rows))
}

async fn do_execute_rust_native(
    client: &tokio_postgres::Client,
    stmt_cache: &mut HashMap<String, Statement>,
    sql: &str,
    params: &[RustSqlValue],
    autocommit: bool,
    in_transaction: &mut bool,
) -> PuffResult<(Statement, Vec<Row>)> {
    if !autocommit && !*in_transaction {
        client.batch_execute("BEGIN").await?;
        *in_transaction = true;
    }

    let stmt = match get_or_prepare(client, stmt_cache, sql).await {
        Ok(s) => s,
        Err(e) => return Err(anyhow!("{e}")),
    };

    let param_refs: Vec<&(dyn ToSql + Sync)> = params
        .iter()
        .map(|p| p as &(dyn ToSql + Sync))
        .collect();

    let rows: Vec<Row> = client
        .query(&stmt, &param_refs)
        .await?;
    Ok((stmt, rows))
}

async fn get_or_prepare(
    client: &tokio_postgres::Client,
    stmt_cache: &mut HashMap<String, Statement>,
    sql: &str,
) -> Result<Statement, PyErr> {
    if let Some(s) = stmt_cache.get(sql) {
        return Ok(s.clone());
    }
    let s = client
        .prepare(sql)
        .await
        .map_err(postgres_to_python_exception)?;
    stmt_cache.insert(sql.to_string(), s.clone());
    Ok(s)
}

// ---------------------------------------------------------------------------
// Parameter placeholder conversion: %s -> $1, $2, ...
// ---------------------------------------------------------------------------

fn convert_placeholders(sql: &str) -> String {
    let mut result = String::with_capacity(sql.len());
    let mut param_num = 0u32;
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            if let Some(&next) = chars.peek() {
                if next == 's' {
                    param_num += 1;
                    result.push('$');
                    result.push_str(&param_num.to_string());
                    chars.next(); // consume 's'
                    continue;
                } else if next == '%' {
                    result.push('%'); // literal %%
                    chars.next();
                    continue;
                }
            }
        }
        result.push(c);
    }
    result
}

// ---------------------------------------------------------------------------
// PostgresGlobal — the entry point from Python: puff.postgres()
// ---------------------------------------------------------------------------

#[pyclass]
pub struct PostgresGlobal;

impl ToPyObject for PostgresGlobal {
    fn to_object(&self, py: Python<'_>) -> PyObject {
        PostgresGlobal.into_py(py)
    }
}

#[pymethods]
impl PostgresGlobal {
    fn __call__(&self) -> Connection {
        let pool = with_puff_context(|c| c.postgres().pool());
        Connection::new(pool)
    }

    fn by_name(&self, name: &str) -> Connection {
        with_puff_context(|ctx| Connection::new(ctx.postgres_named(name).pool()))
    }
}

// ---------------------------------------------------------------------------
// Connection
// ---------------------------------------------------------------------------

#[derive(Clone)]
#[pyclass]
pub struct Connection {
    pool: Pool<PostgresConnectionManager<NoTls>>,
    /// Lazily initialized sender to the background connection task
    sender: Arc<Mutex<Option<Sender<DbOp>>>>,
    closed: Arc<AtomicBool>,
}

impl Connection {
    pub fn new(pool: Pool<PostgresConnectionManager<NoTls>>) -> Self {
        Self {
            pool,
            sender: Arc::new(Mutex::new(None)),
            closed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Returns a pointer-based identity token for this connection.
    /// Two clones of the same `Connection` share the same underlying sender
    /// slot and will therefore return equal identity values.
    pub fn identity(&self) -> usize {
        Arc::as_ptr(&self.sender) as usize
    }
}

/// Get or lazily create the background task sender.
async fn ensure_sender(
    pool: &Pool<PostgresConnectionManager<NoTls>>,
    sender_slot: &Mutex<Option<Sender<DbOp>>>,
) -> Sender<DbOp> {
    let mut guard = sender_slot.lock().await;
    if let Some(ref s) = *guard {
        return s.clone();
    }
    let (tx, rx) = mpsc::channel(32);
    let pool_clone = pool.clone();
    let handle = Handle::try_current().unwrap_or_else(|_| with_puff_context(|ctx| ctx.handle()));
    handle.spawn(async move {
        conn_task(pool_clone, rx).await;
    });
    *guard = Some(tx.clone());
    tx
}

/// Public async helper used by the GraphQL engine.
pub async fn execute_rust(
    conn: &Connection,
    q: String,
    params: Vec<PythonSqlValue>,
) -> PuffResult<(Statement, Vec<Row>)> {
    let sender = ensure_sender(&conn.pool, &conn.sender).await;
    let (tx, rx) = oneshot::channel();
    let converted = convert_placeholders(&q);
    sender
        .send(DbOp::ExecuteRust {
            sql: converted,
            params,
            reply: tx,
        })
        .await
        .map_err(|_| anyhow!("Connection task dropped"))?;
    rx.await.map_err(|_| anyhow!("Connection task dropped"))?
}

/// Public async helper used by the zero-Python GraphQL fast path.
pub async fn execute_rust_native(
    conn: &Connection,
    sql: String,
    params: Vec<RustSqlValue>,
) -> PuffResult<(Statement, Vec<Row>)> {
    let sender = ensure_sender(&conn.pool, &conn.sender).await;
    let (tx, rx) = oneshot::channel();
    sender
        .send(DbOp::ExecuteRustNative {
            sql,
            params,
            reply: tx,
        })
        .await
        .map_err(|_| anyhow!("Connection task dropped"))?;
    rx.await.map_err(|_| anyhow!("Connection task dropped"))?
}

/// Public async helper: set autocommit mode on the background connection task.
pub async fn set_autocommit(conn: &Connection, value: bool) {
    let sender = ensure_sender(&conn.pool, &conn.sender).await;
    let (tx, rx) = oneshot::channel();
    let _ = sender.send(DbOp::SetAutoCommit { value, reply: tx }).await;
    // Best-effort: ignore errors (e.g. task dropped)
    let _ = rx.await;
}

/// Public async helper used by the GraphQL handler to clean up.
pub async fn close_conn(conn: &Connection) {
    let sender = {
        let guard = conn.sender.lock().await;
        guard.clone()
    };
    if let Some(s) = sender {
        let _ = s.send(DbOp::Close).await;
        let mut guard = conn.sender.lock().await;
        *guard = None;
    }
}

#[pymethods]
impl Connection {
    #[getter]
    fn get_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    fn set_auto_commit(&self, ret_func: PyObject, new_autocommit: bool) {
        let pool = self.pool.clone();
        let sender_slot = self.sender.clone();
        let handle = with_puff_context(|ctx| ctx.handle());
        handle.spawn(async move {
            let sender = ensure_sender(&pool, &sender_slot).await;
            let (tx, rx) = oneshot::channel();
            let _ = sender
                .send(DbOp::SetAutoCommit {
                    value: new_autocommit,
                    reply: tx,
                })
                .await;
            handle_python_return(ret_func, async {
                rx.await.map_err(|_| {
                    pyo3::exceptions::PyRuntimeError::new_err("Database operation cancelled")
                })?
            })
            .await;
        });
    }

    fn close(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
        let sender_slot = self.sender.clone();
        let handle = with_puff_context(|ctx| ctx.handle());
        handle.spawn(async move {
            let sender = {
                let mut guard = sender_slot.lock().await;
                guard.take()
            };
            if let Some(s) = sender {
                let _ = s.send(DbOp::Close).await;
            }
        });
    }

    fn commit(&self, return_fun: PyObject) -> PyResult<()> {
        if self.get_closed() {
            return Err(OperationalError::new_err("Cursor has closed."));
        }
        let sender_slot = self.sender.clone();
        let handle = with_puff_context(|ctx| ctx.handle());
        handle.spawn(async move {
            let sender = {
                let guard = sender_slot.lock().await;
                guard.clone()
            };
            if let Some(s) = sender {
                let (tx, rx) = oneshot::channel();
                let _ = s.send(DbOp::Commit { reply: tx }).await;
                handle_python_return(return_fun, async {
                    rx.await.map_err(|_| {
                        pyo3::exceptions::PyRuntimeError::new_err("Database operation cancelled")
                    })?
                })
                .await;
            } else {
                handle_python_return(return_fun, async { Ok(true) }).await;
            }
        });
        Ok(())
    }

    fn rollback(&self, return_fun: PyObject) {
        let sender_slot = self.sender.clone();
        let handle = with_puff_context(|ctx| ctx.handle());
        handle.spawn(async move {
            let sender = {
                let guard = sender_slot.lock().await;
                guard.clone()
            };
            if let Some(s) = sender {
                let (tx, rx) = oneshot::channel();
                let _ = s.send(DbOp::Rollback { reply: tx }).await;
                handle_python_return(return_fun, async {
                    rx.await.map_err(|_| {
                        pyo3::exceptions::PyRuntimeError::new_err("Database operation cancelled")
                    })?
                })
                .await;
            } else {
                handle_python_return(return_fun, async { Ok(true) }).await;
            }
        });
    }

    fn cursor(&self, py: Python) -> PyObject {
        let cursor = Cursor {
            pool: self.pool.clone(),
            sender: self.sender.clone(),
            is_closed: false,
            conn_closed: self.closed.clone(),
            arraysize: 1,
        };
        cursor.into_py(py)
    }
}

// ---------------------------------------------------------------------------
// Cursor
// ---------------------------------------------------------------------------

#[derive(Clone)]
#[pyclass]
pub struct Cursor {
    pool: Pool<PostgresConnectionManager<NoTls>>,
    sender: Arc<Mutex<Option<Sender<DbOp>>>>,
    is_closed: bool,
    conn_closed: Arc<AtomicBool>,
    arraysize: i32,
}

#[pymethods]
impl Cursor {
    #[getter]
    fn get_closed(&self) -> bool {
        self.is_closed || self.conn_closed.load(Ordering::Acquire)
    }

    #[getter]
    fn get_arraysize(&self) -> PyResult<i32> {
        Ok(self.arraysize)
    }

    #[setter]
    fn set_arraysize(&mut self, value: i32) -> PyResult<()> {
        self.arraysize = value;
        Ok(())
    }

    fn setinputsizes(&self, _size: PyObject) -> PyResult<()> {
        Ok(())
    }

    fn setoutputsizes(&self, _size: PyObject, _column: Option<PyObject>) -> PyResult<()> {
        Ok(())
    }

    fn close(&mut self) {
        self.is_closed = true;
    }

    fn callproc(&self, _procname: Bound<'_, PyString>, _parameters: Option<Bound<'_, PyList>>) {}

    fn execute(
        &self,
        py: Python,
        return_func: PyObject,
        operation: String,
        maybe_params: Option<Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        if self.get_closed() {
            return Err(OperationalError::new_err("Cursor has closed."));
        }

        let params = extract_params(py, maybe_params)?;
        let converted_sql = convert_placeholders(&operation);
        let pool = self.pool.clone();
        let sender_slot = self.sender.clone();
        let handle = with_puff_context(|ctx| ctx.handle());

        handle.spawn(async move {
            let sender = ensure_sender(&pool, &sender_slot).await;
            let (tx, rx) = oneshot::channel();
            let _ = sender
                .send(DbOp::Query {
                    sql: converted_sql,
                    params,
                    reply: tx,
                })
                .await;
            handle_python_return::<_, PyObject>(return_func, async move {
                let result: QueryResult = rx.await.map_err(|_| {
                    pyo3::exceptions::PyRuntimeError::new_err("Database operation cancelled")
                })??;
                // Return (rows, description, rowcount) as a Python tuple
                // that the Python wrapper can store on the cursor
                Python::with_gil(|py| {
                    let py_rows = PyList::empty(py);
                    let had_rows = !result.rows.is_empty();
                    // Process rows in batches so each Rust batch is dropped
                    // before the next is converted, reducing peak memory from
                    // O(N) to O(batch_size).
                    let mut rows = result.rows;
                    const BATCH_SIZE: usize = 1000;
                    while !rows.is_empty() {
                        let batch: Vec<Row> = if rows.len() > BATCH_SIZE {
                            rows.drain(..BATCH_SIZE).collect()
                        } else {
                            std::mem::take(&mut rows)
                        };
                        for row in &batch {
                            py_rows.append(row_to_python(py, row)?)?;
                        }
                        // batch is dropped here, freeing Rust memory for those rows
                    }
                    let description = statement_to_description_py(py, &Some(result.statement))?;
                    let rowcount = if had_rows {
                        -1i64
                    } else {
                        result.affected_rows as i64
                    };
                    Ok(PyTuple::new(
                        py,
                        vec![
                            py_rows.into_py(py),
                            description.into_py(py),
                            rowcount.into_py(py),
                        ],
                    )?
                    .into_py(py))
                })
            })
            .await;
        });

        Ok(())
    }

    fn executemany(
        &self,
        py: Python,
        return_func: PyObject,
        operation: String,
        seq_of_parameters: Option<PyObject>,
    ) -> PyResult<()> {
        if self.get_closed() {
            return Err(OperationalError::new_err("Cursor has closed."));
        }

        let param_seq = if let Some(seq) = seq_of_parameters {
            let mut seqs = Vec::new();
            for pyparams in seq.bind(py).iter()? {
                let mut params = Vec::new();
                for pyparam in pyparams?.iter()? {
                    params.push(PythonSqlValue(pyparam?.into_py(py)));
                }
                seqs.push(params)
            }
            seqs
        } else {
            Vec::new()
        };

        let converted_sql = convert_placeholders(&operation);
        let pool = self.pool.clone();
        let sender_slot = self.sender.clone();
        let handle = with_puff_context(|ctx| ctx.handle());

        handle.spawn(async move {
            let sender = ensure_sender(&pool, &sender_slot).await;
            let (tx, rx) = oneshot::channel();
            let _ = sender
                .send(DbOp::ExecuteMany {
                    sql: converted_sql,
                    param_seq,
                    reply: tx,
                })
                .await;
            handle_python_return::<_, PyObject>(return_func, async move {
                let result: QueryResult = rx.await.map_err(|_| {
                    pyo3::exceptions::PyRuntimeError::new_err("Database operation cancelled")
                })??;
                Python::with_gil(|py| {
                    let description = statement_to_description_py(py, &Some(result.statement))?;
                    Ok(PyTuple::new(
                        py,
                        vec![
                            PyList::empty(py).into_py(py),
                            description.into_py(py),
                            (result.affected_rows as i64).into_py(py),
                        ],
                    )?
                    .into_py(py))
                })
            })
            .await;
        });

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers: extract params, row conversion
// ---------------------------------------------------------------------------

fn extract_params(
    py: Python,
    maybe_params: Option<Bound<'_, PyAny>>,
) -> PyResult<Vec<PythonSqlValue>> {
    if let Some(pyparams) = maybe_params {
        let mut params = Vec::new();
        for pyparam in pyparams.iter()? {
            params.push(PythonSqlValue(pyparam?.into_py(py)));
        }
        Ok(params)
    } else {
        Ok(Vec::new())
    }
}

// ---------------------------------------------------------------------------
// column_to_python — convert a single column value from a Row to PyObject
// ---------------------------------------------------------------------------

pub(crate) fn column_to_python(
    py: Python,
    ix: usize,
    c: &Column,
    row: &Row,
) -> PyResult<Py<PyAny>> {
    let val = match *c.type_() {
        Type::BOOL => row.get::<_, Option<bool>>(ix).into_py(py),
        Type::TIME => row.get::<_, Option<NaiveTime>>(ix).into_py(py),
        Type::DATE => row.get::<_, Option<NaiveDate>>(ix).into_py(py),
        Type::TIMESTAMP => row.get::<_, Option<NaiveDateTime>>(ix).into_py(py),
        Type::TIMESTAMPTZ => row
            .get::<_, Option<DateTime<Utc>>>(ix)
            .map(|f| f.naive_local())
            .into_py(py),
        Type::TEXT => row.get::<_, Option<&str>>(ix).into_py(py),
        Type::VARCHAR => row.get::<_, Option<&str>>(ix).into_py(py),
        Type::NAME => row.get::<_, Option<&str>>(ix).into_py(py),
        Type::CHAR => row.get::<_, Option<i8>>(ix).into_py(py),
        Type::UNKNOWN => row.get::<_, Option<&str>>(ix).into_py(py),
        Type::INT2 => row.get::<_, Option<i16>>(ix).into_py(py),
        Type::INT4 => row.get::<_, Option<i32>>(ix).into_py(py),
        Type::INT8 => row.get::<_, Option<i64>>(ix).into_py(py),
        Type::FLOAT4 => row.get::<_, Option<f32>>(ix).into_py(py),
        Type::FLOAT8 => row.get::<_, Option<f64>>(ix).into_py(py),
        Type::OID => row.get::<_, Option<u32>>(ix).into_py(py),
        Type::BYTEA => row.get::<_, Option<&[u8]>>(ix).into_py(py),
        Type::UUID => pythonize::pythonize(py, &row.get::<_, Option<Uuid>>(ix))?.unbind(),
        Type::JSON => {
            pythonize::pythonize(py, &row.get::<_, Option<serde_json::Value>>(ix))?.unbind()
        }
        Type::JSONB => {
            pythonize::pythonize(py, &row.get::<_, Option<serde_json::Value>>(ix))?.unbind()
        }
        Type::BOOL_ARRAY => row.get::<_, Option<Vec<Option<bool>>>>(ix).into_py(py),
        Type::TEXT_ARRAY => row.get::<_, Option<Vec<Option<&str>>>>(ix).into_py(py),
        Type::VARCHAR_ARRAY => row.get::<_, Option<Vec<Option<&str>>>>(ix).into_py(py),
        Type::NAME_ARRAY => row.get::<_, Option<Vec<Option<&str>>>>(ix).into_py(py),
        Type::CHAR_ARRAY => row.get::<_, Option<Vec<Option<i8>>>>(ix).into_py(py),
        Type::INT2_ARRAY => row.get::<_, Option<Vec<Option<i16>>>>(ix).into_py(py),
        Type::INT4_ARRAY => row.get::<_, Option<Vec<Option<i32>>>>(ix).into_py(py),
        Type::INT8_ARRAY => row.get::<_, Option<Vec<Option<i64>>>>(ix).into_py(py),
        Type::FLOAT4_ARRAY => row.get::<_, Option<Vec<Option<f32>>>>(ix).into_py(py),
        Type::FLOAT8_ARRAY => row.get::<_, Option<Vec<Option<f64>>>>(ix).into_py(py),
        Type::OID_ARRAY => row.get::<_, Option<Vec<Option<u32>>>>(ix).into_py(py),
        Type::BYTEA_ARRAY => row.get::<_, Option<Vec<Option<&[u8]>>>>(ix).into_py(py),
        Type::UUID_ARRAY => {
            pythonize::pythonize(py, &row.get::<_, Vec<Option<Uuid>>>(ix))?.unbind()
        }
        Type::TIME_ARRAY => row.get::<_, Option<Vec<Option<NaiveTime>>>>(ix).into_py(py),
        Type::DATE_ARRAY => row.get::<_, Option<Vec<Option<NaiveDate>>>>(ix).into_py(py),
        ref t => {
            return Err(NotSupportedError::new_err(format!(
                "Unsupported postgres type {:?}",
                t
            )))
        }
    };
    Ok(val)
}

fn row_to_python(py: Python, row: &Row) -> PyResult<Py<PyTuple>> {
    let mut row_vec = Vec::with_capacity(row.len());

    for (ix, c) in row.columns().iter().enumerate() {
        let val = column_to_python(py, ix, c, row)?;
        row_vec.push(val)
    }
    Ok(PyTuple::new(py, row_vec)?.into_py(py))
}

// ---------------------------------------------------------------------------
// RustSqlValue — pure-Rust SQL parameter (no Python GIL needed)
// ---------------------------------------------------------------------------

/// A SQL parameter value that lives entirely in Rust — no Python objects.
/// Used by the zero-Python fast path for `@sql`-annotated GraphQL fields.
#[derive(Debug, Clone)]
pub enum RustSqlValue {
    Null,
    Bool(bool),
    Int(i32),
    Long(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
    Uuid(Uuid),
    Json(serde_json::Value),
    /// Array of i32 (for ANY($1) with int arrays)
    IntArray(Vec<i32>),
    /// Array of i64
    LongArray(Vec<i64>),
    /// Array of String
    TextArray(Vec<String>),
    /// Array of GqlValue serialized to serde_json::Value (for heterogeneous arrays)
    JsonArray(Vec<serde_json::Value>),
}

impl ToSql for RustSqlValue {
    fn to_sql(
        &self,
        ty: &Type,
        out: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn Error + Sync + Send>> {
        match self {
            RustSqlValue::Null => Ok(IsNull::Yes),
            RustSqlValue::Bool(b) => b.to_sql(ty, out),
            RustSqlValue::Int(i) => match *ty {
                Type::INT2 => (*i as i16).to_sql(ty, out),
                Type::INT8 => (*i as i64).to_sql(ty, out),
                _ => i.to_sql(ty, out),
            },
            RustSqlValue::Long(l) => match *ty {
                Type::INT4 => (*l as i32).to_sql(ty, out),
                Type::INT2 => (*l as i16).to_sql(ty, out),
                _ => l.to_sql(ty, out),
            },
            RustSqlValue::Float(f) => match *ty {
                Type::FLOAT4 => (*f as f32).to_sql(ty, out),
                _ => f.to_sql(ty, out),
            },
            RustSqlValue::Text(s) => s.to_sql(ty, out),
            RustSqlValue::Bytes(b) => b.as_slice().to_sql(ty, out),
            RustSqlValue::Uuid(u) => u.to_sql(ty, out),
            RustSqlValue::Json(v) => v.to_sql(ty, out),
            RustSqlValue::IntArray(a) => a.to_sql(ty, out),
            RustSqlValue::LongArray(a) => a.to_sql(ty, out),
            RustSqlValue::TextArray(a) => a.to_sql(ty, out),
            RustSqlValue::JsonArray(a) => serde_json::Value::Array(a.clone()).to_sql(ty, out),
        }
    }

    fn accepts(ty: &Type) -> bool
    where
        Self: Sized,
    {
        // Accept everything PythonSqlValue accepts, plus arrays
        matches!(
            *ty,
            Type::BOOL
                | Type::INT2
                | Type::INT4
                | Type::INT8
                | Type::FLOAT4
                | Type::FLOAT8
                | Type::TEXT
                | Type::VARCHAR
                | Type::NAME
                | Type::CHAR
                | Type::UNKNOWN
                | Type::BYTEA
                | Type::UUID
                | Type::JSON
                | Type::JSONB
                | Type::OID
                | Type::INT4_ARRAY
                | Type::INT8_ARRAY
                | Type::TEXT_ARRAY
                | Type::VARCHAR_ARRAY
                | Type::BOOL_ARRAY
                | Type::FLOAT4_ARRAY
                | Type::FLOAT8_ARRAY
        )
    }

    to_sql_checked!();
}

/// Convert an `async_graphql::Value` to a `RustSqlValue` without touching Python.
pub fn gql_value_to_rust_sql(v: &async_graphql::Value) -> RustSqlValue {
    match v {
        async_graphql::Value::Null => RustSqlValue::Null,
        async_graphql::Value::Boolean(b) => RustSqlValue::Bool(*b),
        async_graphql::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                if i >= i32::MIN as i64 && i <= i32::MAX as i64 {
                    RustSqlValue::Int(i as i32)
                } else {
                    RustSqlValue::Long(i)
                }
            } else if let Some(f) = n.as_f64() {
                RustSqlValue::Float(f)
            } else {
                RustSqlValue::Null
            }
        }
        async_graphql::Value::String(s) => {
            // Try to parse as UUID first
            if let Ok(u) = Uuid::parse_str(s) {
                RustSqlValue::Uuid(u)
            } else {
                RustSqlValue::Text(s.clone())
            }
        }
        async_graphql::Value::List(items) => {
            // Try to infer a typed array from the first non-null element
            let first_non_null = items.iter().find(|v| !matches!(v, async_graphql::Value::Null));
            match first_non_null {
                Some(async_graphql::Value::Number(n)) => {
                    if n.as_i64().is_some() {
                        // Check if all fit in i32
                        let all_i32 = items.iter().all(|v| match v {
                            async_graphql::Value::Number(n) => {
                                n.as_i64().is_some_and(|i| i >= i32::MIN as i64 && i <= i32::MAX as i64)
                            }
                            async_graphql::Value::Null => true,
                            _ => false,
                        });
                        if all_i32 {
                            RustSqlValue::IntArray(
                                items
                                    .iter()
                                    .map(|v| match v {
                                        async_graphql::Value::Number(n) => n.as_i64().unwrap_or(0) as i32,
                                        _ => 0,
                                    })
                                    .collect(),
                            )
                        } else {
                            RustSqlValue::LongArray(
                                items
                                    .iter()
                                    .map(|v| match v {
                                        async_graphql::Value::Number(n) => n.as_i64().unwrap_or(0),
                                        _ => 0,
                                    })
                                    .collect(),
                            )
                        }
                    } else {
                        RustSqlValue::Json(gql_value_to_json(v))
                    }
                }
                Some(async_graphql::Value::String(_)) => {
                    RustSqlValue::TextArray(
                        items
                            .iter()
                            .map(|v| match v {
                                async_graphql::Value::String(s) => s.clone(),
                                _ => String::new(),
                            })
                            .collect(),
                    )
                }
                _ => RustSqlValue::Json(gql_value_to_json(v)),
            }
        }
        async_graphql::Value::Object(_) => RustSqlValue::Json(gql_value_to_json(v)),
        _ => RustSqlValue::Null,
    }
}

/// Convert an async_graphql::Value to serde_json::Value.
fn gql_value_to_json(v: &async_graphql::Value) -> serde_json::Value {
    match v {
        async_graphql::Value::Null => serde_json::Value::Null,
        async_graphql::Value::Boolean(b) => serde_json::Value::Bool(*b),
        async_graphql::Value::Number(n) => serde_json::Value::Number(n.clone()),
        async_graphql::Value::String(s) => serde_json::Value::String(s.clone()),
        async_graphql::Value::List(items) => {
            serde_json::Value::Array(items.iter().map(gql_value_to_json).collect())
        }
        async_graphql::Value::Object(obj) => {
            let map: serde_json::Map<String, serde_json::Value> = obj
                .iter()
                .map(|(k, v)| (k.to_string(), gql_value_to_json(v)))
                .collect();
            serde_json::Value::Object(map)
        }
        _ => serde_json::Value::Null,
    }
}

// ---------------------------------------------------------------------------
// PythonSqlValue — wraps a PyObject for use as a tokio-postgres parameter
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct PythonSqlValue(PyObject);

impl Clone for PythonSqlValue {
    fn clone(&self) -> Self {
        Python::with_gil(|py| PythonSqlValue(self.0.clone_ref(py)))
    }
}

impl PythonSqlValue {
    pub fn new(value: PyObject) -> PythonSqlValue {
        PythonSqlValue(value)
    }
}

impl ToSql for PythonSqlValue {
    fn to_sql(&self, ty: &Type, out: &mut BytesMut) -> Result<IsNull, Box<dyn Error + Sync + Send>>
    where
        Self: Sized,
    {
        Python::with_gil(|py| {
            let obj_ref = self.0.bind(py);
            match *ty {
                Type::JSON => depythonize::<Option<serde_json::Value>>(obj_ref)?.to_sql(ty, out),
                Type::JSONB => depythonize::<Option<serde_json::Value>>(obj_ref)?.to_sql(ty, out),
                Type::TIMESTAMP => obj_ref.extract::<Option<NaiveDateTime>>()?.to_sql(ty, out),
                Type::TIMESTAMPTZ => obj_ref.extract::<Option<NaiveDateTime>>()?.to_sql(ty, out),
                Type::BOOL => obj_ref.extract::<Option<bool>>()?.to_sql(ty, out),
                Type::TEXT => {
                    let s = obj_ref.extract::<Option<String>>();
                    match s {
                        Ok(s) => s.to_sql(ty, out),
                        Err(_) => obj_ref.to_string().to_sql(ty, out),
                    }
                }
                Type::VARCHAR => obj_ref.extract::<Option<String>>()?.to_sql(ty, out),
                Type::NAME => obj_ref.extract::<Option<String>>()?.to_sql(ty, out),
                Type::CHAR => obj_ref.extract::<Option<i8>>()?.to_sql(ty, out),
                Type::UNKNOWN => obj_ref.extract::<Option<String>>()?.to_sql(ty, out),
                Type::INT2 => obj_ref.extract::<Option<i16>>()?.to_sql(ty, out),
                Type::INT4 => obj_ref.extract::<Option<i32>>()?.to_sql(ty, out),
                Type::INT8 => obj_ref.extract::<Option<i64>>()?.to_sql(ty, out),
                Type::FLOAT4 => obj_ref.extract::<Option<f32>>()?.to_sql(ty, out),
                Type::FLOAT8 => obj_ref.extract::<Option<f64>>()?.to_sql(ty, out),
                Type::OID => obj_ref.extract::<Option<u32>>()?.to_sql(ty, out),
                Type::BYTEA => obj_ref.extract::<Option<Vec<u8>>>()?.to_sql(ty, out),
                Type::BOOL_ARRAY => obj_ref.extract::<Option<Vec<bool>>>()?.to_sql(ty, out),
                Type::TEXT_ARRAY => obj_ref.extract::<Option<Vec<String>>>()?.to_sql(ty, out),
                Type::VARCHAR_ARRAY => obj_ref.extract::<Option<Vec<String>>>()?.to_sql(ty, out),
                Type::NAME_ARRAY => obj_ref.extract::<Option<Vec<String>>>()?.to_sql(ty, out),
                Type::CHAR_ARRAY => obj_ref.extract::<Option<Vec<i8>>>()?.to_sql(ty, out),
                Type::INT2_ARRAY => obj_ref.extract::<Option<Vec<i16>>>()?.to_sql(ty, out),
                Type::INT4_ARRAY => obj_ref.extract::<Option<Vec<i32>>>()?.to_sql(ty, out),
                Type::INT8_ARRAY => obj_ref.extract::<Option<Vec<i64>>>()?.to_sql(ty, out),
                Type::FLOAT4_ARRAY => obj_ref.extract::<Option<Vec<f32>>>()?.to_sql(ty, out),
                Type::FLOAT8_ARRAY => obj_ref.extract::<Option<Vec<f64>>>()?.to_sql(ty, out),
                Type::OID_ARRAY => obj_ref.extract::<Option<Vec<u32>>>()?.to_sql(ty, out),
                Type::BYTEA_ARRAY => obj_ref.extract::<Option<Vec<Vec<u8>>>>()?.to_sql(ty, out),
                ref t => Err(anyhow!(
                    "Could not convert postgres type {:?} from python {:?}",
                    t,
                    obj_ref
                )
                .into()),
            }
        })
    }
    fn accepts(ty: &Type) -> bool
    where
        Self: Sized,
    {
        matches!(
            *ty,
            Type::JSON
                | Type::JSONB
                | Type::TIMESTAMP
                | Type::TIMESTAMPTZ
                | Type::BOOL
                | Type::TEXT
                | Type::VARCHAR
                | Type::NAME
                | Type::CHAR
                | Type::UNKNOWN
                | Type::INT2
                | Type::INT4
                | Type::INT8
                | Type::FLOAT4
                | Type::FLOAT8
                | Type::OID
                | Type::UUID
                | Type::BYTEA
                | Type::BOOL_ARRAY
                | Type::TEXT_ARRAY
                | Type::VARCHAR_ARRAY
                | Type::NAME_ARRAY
                | Type::CHAR_ARRAY
                | Type::INT2_ARRAY
                | Type::INT4_ARRAY
                | Type::INT8_ARRAY
                | Type::FLOAT4_ARRAY
                | Type::FLOAT8_ARRAY
                | Type::OID_ARRAY
                | Type::UUID_ARRAY
                | Type::BYTEA_ARRAY
        )
    }

    to_sql_checked!();
}

// ---------------------------------------------------------------------------
// postgres_to_python_exception — map tokio_postgres errors to Python exceptions
// ---------------------------------------------------------------------------

fn postgres_to_python_exception(e: tokio_postgres::Error) -> PyErr {
    if e.is_closed() {
        return InternalError::new_err(format!(
            "Error occurred because connection is closed: {:?}",
            e
        ));
    }
    match e.as_db_error() {
        Some(db_err) => {
            let message = db_err.message();
            let hint = db_err
                .hint()
                .map(|f| format!(" Hint: {}", f))
                .unwrap_or_default();
            let severity = db_err.severity();
            let pos = db_err
                .hint()
                .map(|f| format!(" Position: {}", f))
                .unwrap_or_default();
            let schema = db_err
                .schema()
                .map(|f| format!(" Schema: {}", f))
                .unwrap_or_default();
            let table = db_err
                .table()
                .map(|f| format!(" Table: {}", f))
                .unwrap_or_default();
            let column = db_err
                .column()
                .map(|f| format!(" Column: {}", f))
                .unwrap_or_default();
            let constraint = db_err
                .constraint()
                .map(|f| format!(" Constraint: {}", f))
                .unwrap_or_default();
            let formatted = format!(
                "Message: {}, Severity: {}{}{}{}{}{}{}",
                message, severity, hint, pos, schema, table, column, constraint
            );
            match *db_err.code() {
                SqlState::WARNING => Warning::new_err(formatted),
                SqlState::SYNTAX_ERROR => ProgrammingError::new_err(formatted),
                SqlState::INVALID_TABLE_DEFINITION => ProgrammingError::new_err(formatted),
                SqlState::INTEGRITY_CONSTRAINT_VIOLATION => IntegrityError::new_err(formatted),
                SqlState::FOREIGN_KEY_VIOLATION => IntegrityError::new_err(formatted),
                SqlState::UNDEFINED_TABLE => IntegrityError::new_err(formatted),
                SqlState::INVALID_FOREIGN_KEY => IntegrityError::new_err(formatted),
                SqlState::NOT_NULL_VIOLATION => IntegrityError::new_err(formatted),
                SqlState::DISK_FULL => OperationalError::new_err(formatted),
                SqlState::DATA_CORRUPTED => OperationalError::new_err(formatted),
                SqlState::FEATURE_NOT_SUPPORTED => NotSupportedError::new_err(formatted),
                SqlState::NO_ACTIVE_SQL_TRANSACTION => NotSupportedError::new_err(formatted),
                SqlState::INVALID_DATETIME_FORMAT => DataError::new_err(formatted),
                SqlState::INVALID_ARGUMENT_FOR_LOG => DataError::new_err(formatted),
                SqlState::INVALID_ARGUMENT_FOR_NTH_VALUE => DataError::new_err(formatted),
                SqlState::INVALID_ARGUMENT_FOR_SQL_JSON_DATETIME_FUNCTION => {
                    DataError::new_err(formatted)
                }
                SqlState::INVALID_ARGUMENT_FOR_NTILE => DataError::new_err(formatted),
                SqlState::INVALID_ARGUMENT_FOR_POWER_FUNCTION => DataError::new_err(formatted),
                SqlState::INVALID_ARGUMENT_FOR_WIDTH_BUCKET_FUNCTION => {
                    DataError::new_err(formatted)
                }
                SqlState::NUMERIC_VALUE_OUT_OF_RANGE => DataError::new_err(formatted),
                SqlState::DATETIME_FIELD_OVERFLOW => DataError::new_err(formatted),
                SqlState::NULL_VALUE_NOT_ALLOWED => DataError::new_err(formatted),
                _ => InternalError::new_err(formatted),
            }
        }
        None => InternalError::new_err(format!("{:?}", e)),
    }
}

// ---------------------------------------------------------------------------
// pg_type_to_db2_type / statement_to_description — DB-API 2.0 type objects
// ---------------------------------------------------------------------------

fn pg_type_to_db2_type(py: Python, ty: &Type) -> PyResult<PyObject> {
    match *ty {
        Type::TIME => get_cached_object(py, "puff.postgres.TIME".into()),
        Type::DATE => get_cached_object(py, "puff.postgres.DATE".into()),
        Type::TIMESTAMP => get_cached_object(py, "puff.postgres.DATETIME".into()),
        Type::TIMESTAMPTZ => get_cached_object(py, "puff.postgres.DATETIME".into()),
        Type::TEXT => get_cached_object(py, "puff.postgres.STRING".into()),
        Type::VARCHAR => get_cached_object(py, "puff.postgres.STRING".into()),
        Type::INT2 => get_cached_object(py, "puff.postgres.NUMBER".into()),
        Type::INT4 => get_cached_object(py, "puff.postgres.NUMBER".into()),
        Type::INT8 => get_cached_object(py, "puff.postgres.NUMBER".into()),
        Type::FLOAT4 => get_cached_object(py, "puff.postgres.NUMBER".into()),
        Type::FLOAT8 => get_cached_object(py, "puff.postgres.NUMBER".into()),
        Type::OID => get_cached_object(py, "puff.postgres.ROWID".into()),
        Type::BYTEA => get_cached_object(py, "puff.postgres.BINARY".into()),
        ref t => {
            get_cached_object(py, "puff.postgres.TypeObject".into())?.call1(py, (t.to_string(),))
        }
    }
}

fn statement_to_description_py(
    py: Python,
    statement: &Option<Statement>,
) -> PyResult<Option<PyObject>> {
    match statement.as_ref() {
        Some(st) => {
            let cols = st.columns();
            if cols.is_empty() {
                Ok(None)
            } else {
                let desc = PyList::empty(py);
                for col in cols {
                    let col_t = pg_type_to_db2_type(py, col.type_())?;
                    let tup = PyTuple::new(
                        py,
                        vec![
                            col.name().into_py(py),
                            col_t,
                            py.None(),
                            py.None(),
                            py.None(),
                            py.None(),
                            py.None(),
                        ],
                    )?;
                    desc.append(tup)?;
                }
                Ok(Some(desc.into_py(py)))
            }
        }
        None => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// add_pg_puff_exceptions — register exception types into the puff.postgres module
// ---------------------------------------------------------------------------

pub fn add_pg_puff_exceptions(py: Python) -> PyResult<()> {
    let puff_pg = py.import("puff.postgres")?;
    puff_pg.add("Error", py.get_type::<PgError>())?;
    puff_pg.add("Warning", py.get_type::<Warning>())?;
    puff_pg.add("InternalError", py.get_type::<InternalError>())?;
    puff_pg.add("InterfaceError", py.get_type::<InterfaceError>())?;
    puff_pg.add("DatabaseError", py.get_type::<DatabaseError>())?;
    puff_pg.add("OperationalError", py.get_type::<OperationalError>())?;
    puff_pg.add("ProgrammingError", py.get_type::<ProgrammingError>())?;
    puff_pg.add("IntegrityError", py.get_type::<IntegrityError>())?;
    puff_pg.add("DataError", py.get_type::<DataError>())?;
    puff_pg.add("NotSupportedError", py.get_type::<NotSupportedError>())?;
    Ok(())
}
