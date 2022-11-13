use crate::context::with_puff_context;
use crate::errors::{handle_puff_error, PuffResult};
use crate::python::async_python::{handle_python_return, handle_return};
use crate::python::{get_cached_object, log_traceback_with_label};

use anyhow::anyhow;
use bb8_postgres::bb8::Pool;

use bb8_postgres::PostgresConnectionManager;
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use futures_util::{StreamExt, TryStreamExt};
use pyo3::create_exception;

use pyo3::prelude::*;
use pyo3::types::{PyList, PyString, PyTuple};
use pythonize::depythonize;
use std::error::Error;
use std::fmt::{Debug, Formatter};
use std::future::{ready, Future};
use std::pin::Pin;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio::sync::{oneshot, Mutex};
use tokio_postgres::error::SqlState;
use tokio_postgres::types::private::BytesMut;
use tokio_postgres::types::{to_sql_checked, IsNull, ToSql, Type};
use tokio_postgres::{Client, Column, NoTls, Portal, Row, RowStream, Statement, Transaction};
use uuid::Uuid;

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

#[derive(Clone)]
#[pyclass]
pub struct Connection {
    pool: Pool<PostgresConnectionManager<NoTls>>,
    transaction_loop: Arc<Mutex<Option<Sender<TxnCommand>>>>,
    closed: Arc<AtomicBool>,
}

async fn get_sender(
    loop_pool: Pool<PostgresConnectionManager<NoTls>>,
    transaction_loop: Arc<Mutex<Option<Sender<TxnCommand>>>>,
) -> Sender<TxnCommand> {
    let mut x = transaction_loop.lock().await;
    if let Some(l) = x.as_ref() {
        l.clone()
    } else {
        let (sender, rec) = channel(1);
        *x = Some(sender.clone());
        let handle = with_puff_context(|ctx| ctx.handle());
        handle.spawn(async move { run_loop(loop_pool, rec).await.unwrap() });
        sender
    }
}

pub async fn execute_rust(
    conn: &Connection,
    q: String,
    params: Vec<PythonSqlValue>,
) -> PuffResult<(Statement, Vec<Row>)> {
    let transaction_loop_inner = conn.transaction_loop.clone();
    let sender = get_sender(conn.pool.clone(), transaction_loop_inner).await;
    let (one_sender, rec) = oneshot::channel();
    sender
        .send(TxnCommand::ExecuteRust(one_sender, q, params))
        .await?;
    rec.await?
}

pub async fn close_conn(conn: &Connection) {
    let transaction_loop_inner = &conn.transaction_loop;
    let sender = {
        let job_sender = transaction_loop_inner.lock().await;
        job_sender.clone()
    };
    if let Some(s) = sender {
        s.send(TxnCommand::Close(transaction_loop_inner.clone()))
            .await
            .unwrap_or_default();
    }
}

impl Connection {
    pub fn new(pool: Pool<PostgresConnectionManager<NoTls>>) -> Self {
        Self {
            pool,
            closed: Arc::new(AtomicBool::new(false)),
            transaction_loop: Arc::new(Mutex::new(None)),
        }
    }
}

#[pymethods]
impl Connection {
    #[getter]
    fn get_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    fn set_auto_commit(&self, ret_func: PyObject, new_autocommit: bool) {
        let ctx = with_puff_context(|ctx| ctx);
        let loop_pool = self.pool.clone();
        let transaction_loop_inner = self.transaction_loop.clone();
        ctx.handle().spawn(async move {
            let sender = get_sender(loop_pool, transaction_loop_inner).await;
            sender
                .send(TxnCommand::SetAutoCommit(ret_func, new_autocommit))
                .await
        });
    }

    fn close(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
        let ctx = with_puff_context(|ctx| ctx);
        let transaction_loop_inner = self.transaction_loop.clone();
        ctx.handle().spawn(async move {
            let sender = {
                let local_loop = transaction_loop_inner.clone();
                let job_sender = local_loop.lock().await;
                job_sender.clone()
            };

            if let Some(s) = sender {
                s.send(TxnCommand::Close(transaction_loop_inner)).await
            } else {
                return Ok(());
            }
        });
    }
    fn commit(&self, return_fun: PyObject) -> PyResult<()> {
        if self.get_closed() {
            return Err(OperationalError::new_err("Cursor has closed."));
        }
        let ctx = with_puff_context(|ctx| ctx);
        let transaction_loop_inner = self.transaction_loop.clone();
        ctx.handle().spawn(async move {
            let sender = {
                let job_sender = transaction_loop_inner.lock().await;
                job_sender.clone()
            };
            if let Some(s) = sender {
                s.send(TxnCommand::Commit(return_fun)).await
            } else {
                handle_return(return_fun, async { Ok(()) }).await;
                return Ok(());
            }
        });

        Ok(())
    }
    fn rollback(&self, return_fun: PyObject) {
        let ctx = with_puff_context(|ctx| ctx);
        let transaction_loop_inner = self.transaction_loop.clone();
        ctx.handle().spawn(async move {
            let sender = {
                let job_sender = transaction_loop_inner.lock().await;
                job_sender.clone()
            };

            if let Some(s) = sender {
                s.send(TxnCommand::Rollback(return_fun)).await
            } else {
                handle_return(return_fun, async { Ok(()) }).await;
                return Ok(());
            }
        });
    }
    fn cursor(&self, py: Python) -> PyObject {
        let cursor = Cursor {
            pool: self.pool.clone(),
            is_closed: false,
            conn_closed: self.closed.clone(),
            transaction_loop: self.transaction_loop.clone(),
            arraysize: 1,
        };
        cursor.into_py(py)
    }
}

#[derive(Clone)]
#[pyclass]
pub struct Cursor {
    pool: Pool<PostgresConnectionManager<NoTls>>,
    transaction_loop: Arc<Mutex<Option<Sender<TxnCommand>>>>,
    is_closed: bool,
    conn_closed: Arc<AtomicBool>,
    arraysize: i32,
}

impl Cursor {
    fn spawn_and_recover<F: Future<Output = PuffResult<()>> + Send + Sync + 'static>(
        &self,
        return_fun: PyObject,
        f: F,
    ) {
        let ctx = with_puff_context(|ctx| ctx);
        ctx.handle().spawn(async move {
            let res = f.await;
            if let Err(e) = res {
                Python::with_gil(|py| {
                    return_fun
                        .call1(
                            py,
                            (
                                py.None(),
                                InternalError::new_err("Error sending cursor command"),
                            ),
                        )
                        .map_err(|e| log_traceback_with_label("Postgres return", &e))
                        .unwrap()
                });
                handle_puff_error("Postgres", e);
            }
        });
    }
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

    fn do_get_rowcount(&self, return_fun: PyObject) {
        let inner_loop = self.transaction_loop.clone();
        self.spawn_and_recover(return_fun.clone(), async move {
            let txn_loop = {
                let m = inner_loop.lock().await;
                m.clone()
            };
            match txn_loop {
                Some(job_sender) => Ok(job_sender.send(TxnCommand::RowCount(return_fun)).await?),
                None => {
                    Python::with_gil(|py| return_fun.call1(py, (py.None(), py.None())))?;
                    Ok(())
                }
            }
        })
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

        let seqs = if let Some(seq) = seq_of_parameters {
            let mut seqs = Vec::new();
            for pyparams in seq.as_ref(py).iter()? {
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

        let this_self = self.clone();

        self.spawn_and_recover(return_func.clone(), async move {
            let job_sender = get_sender(this_self.pool, this_self.transaction_loop).await;
            Ok(job_sender
                .send(TxnCommand::ExecuteMany(return_func, operation, seqs))
                .await?)
        });

        Ok(())
    }

    fn execute(
        &self,
        py: Python,
        return_func: PyObject,
        operation: String,
        maybe_params: Option<&PyAny>,
    ) -> PyResult<()> {
        if self.get_closed() {
            return Err(OperationalError::new_err("Cursor has closed."));
        }

        let sql_p = if let Some(pyparams) = maybe_params {
            let mut params = Vec::new();
            for pyparam in pyparams.iter()? {
                params.push(PythonSqlValue(pyparam?.into_py(py)));
            }
            params
        } else {
            Vec::new()
        };

        let this_self = self.clone();
        self.spawn_and_recover(return_func.clone(), async move {
            let job_sender = get_sender(this_self.pool, this_self.transaction_loop).await;
            Ok(job_sender
                .send(TxnCommand::Execute(return_func, operation, sql_p))
                .await?)
        });

        Ok(())
    }

    fn callproc(&self, _procname: &PyString, _parameters: Option<&PyList>) {}

    fn description(&mut self, return_func: PyObject) {
        let this_self = self.clone();
        self.spawn_and_recover(return_func.clone(), async move {
            let job_sender = get_sender(this_self.pool, this_self.transaction_loop).await;
            Ok(job_sender
                .send(TxnCommand::Description(return_func))
                .await?)
        });
    }

    fn fetchone(&mut self, return_func: PyObject) {
        let this_self = self.clone();
        self.spawn_and_recover(return_func.clone(), async move {
            let job_sender = get_sender(this_self.pool, this_self.transaction_loop).await;
            Ok(job_sender.send(TxnCommand::FetchOne(return_func)).await?)
        });
    }

    fn fetchmany(&mut self, return_func: PyObject, rowcount: Option<i32>) {
        let real_row_count = rowcount.unwrap_or(self.arraysize);
        let this_self = self.clone();
        self.spawn_and_recover(return_func.clone(), async move {
            let job_sender = get_sender(this_self.pool, this_self.transaction_loop).await;
            Ok(job_sender
                .send(TxnCommand::FetchMany(return_func, real_row_count))
                .await?)
        });
    }

    fn fetchall(&mut self, return_func: PyObject) {
        let this_self = self.clone();
        self.spawn_and_recover(return_func.clone(), async move {
            let job_sender = get_sender(this_self.pool, this_self.transaction_loop).await;
            Ok(job_sender.send(TxnCommand::FetchAll(return_func)).await?)
        });
    }
}

pub(crate) fn column_to_python(
    py: Python,
    ix: usize,
    c: &Column,
    row: &Row,
) -> PyResult<Py<PyAny>> {
    let val = match c.type_().clone() {
        Type::BOOL => row.get::<_, Option<bool>>(ix).into_py(py),
        Type::TIME => row
            .get::<_, Option<NaiveTime>>(ix)
            .map(|f| pyo3_chrono::NaiveTime::from(f))
            .into_py(py),
        Type::DATE => row
            .get::<_, Option<NaiveDate>>(ix)
            .map(|f| pyo3_chrono::NaiveDate::from(f))
            .into_py(py),
        Type::TIMESTAMP => row
            .get::<_, Option<NaiveDateTime>>(ix)
            .map(|f| pyo3_chrono::NaiveDateTime::from(f))
            .into_py(py),
        Type::TIMESTAMPTZ => row
            .get::<_, Option<DateTime<Utc>>>(ix)
            .map(|f| pyo3_chrono::NaiveDateTime::from(f.naive_local()))
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
        Type::UUID => pythonize::pythonize(py, &row.get::<_, Option<Uuid>>(ix))?,
        Type::JSON => pythonize::pythonize(py, &row.get::<_, Option<serde_json::Value>>(ix))?,
        Type::JSONB => pythonize::pythonize(py, &row.get::<_, Option<serde_json::Value>>(ix))?,
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
        Type::UUID_ARRAY => pythonize::pythonize(py, &row.get::<_, Vec<Option<Uuid>>>(ix))?,
        Type::TIME_ARRAY => row
            .get::<_, Option<Vec<Option<NaiveTime>>>>(ix)
            .map(|f| {
                f.into_iter()
                    .map(|v| v.map(pyo3_chrono::NaiveTime))
                    .collect::<Vec<_>>()
            })
            .into_py(py),
        Type::DATE_ARRAY => row
            .get::<_, Option<Vec<Option<NaiveDate>>>>(ix)
            .map(|f| {
                f.into_iter()
                    .map(|v| v.map(pyo3_chrono::NaiveDate::from))
                    .collect::<Vec<_>>()
            })
            .into_py(py),
        t => {
            return Err(NotSupportedError::new_err(format!(
                "Unsupported postgres type {:?}",
                t
            )))
        }
    };
    Ok(val)
}

fn row_to_pyton(py: Python, row: Row) -> PyResult<Py<PyTuple>> {
    let mut row_vec = Vec::with_capacity(row.len());

    for (ix, c) in row.columns().iter().enumerate() {
        let val = column_to_python(py, ix, c, &row)?;
        row_vec.push(val)
    }
    Ok(PyTuple::new(py, row_vec).into_py(py))
}

enum TxnCommand {
    Execute(PyObject, String, Vec<PythonSqlValue>),
    ExecuteRust(
        oneshot::Sender<PuffResult<(Statement, Vec<Row>)>>,
        String,
        Vec<PythonSqlValue>,
    ),
    ExecuteMany(PyObject, String, Vec<Vec<PythonSqlValue>>),
    FetchOne(PyObject),
    FetchMany(PyObject, i32),
    FetchAll(PyObject),
    Commit(PyObject),
    Rollback(PyObject),
    RowCount(PyObject),
    SetAutoCommit(PyObject, bool),
    Description(PyObject),
    Close(Arc<Mutex<Option<Sender<TxnCommand>>>>),
}

impl Debug for TxnCommand {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            TxnCommand::Execute(_, _, _) => f.write_str("Execute"),
            TxnCommand::ExecuteRust(_, _, _) => f.write_str("ExecuteRust"),
            TxnCommand::ExecuteMany(_, _, _) => f.write_str("ExecuteMany"),
            TxnCommand::FetchOne(_) => f.write_str("FetchOne"),
            TxnCommand::FetchMany(_, _) => f.write_str("FetchMany"),
            TxnCommand::FetchAll(_) => f.write_str("FetchAll"),
            TxnCommand::Commit(_) => f.write_str("Commit"),
            TxnCommand::Rollback(_) => f.write_str("Rollback"),
            TxnCommand::RowCount(_) => f.write_str("RowCount"),
            TxnCommand::SetAutoCommit(_, _) => f.write_str("SetAutoCommit"),
            TxnCommand::Description(_) => f.write_str("Description"),
            TxnCommand::Close(_) => f.write_str("Close"),
        }
    }
}
#[derive(Debug, Clone)]
pub struct PythonSqlValue(PyObject);

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
            let obj_ref = self.0.as_ref(py);
            match ty.clone() {
                Type::JSON => depythonize::<Option<serde_json::Value>>(obj_ref)?.to_sql(ty, out),
                Type::JSONB => depythonize::<Option<serde_json::Value>>(obj_ref)?.to_sql(ty, out),
                Type::TIMESTAMP => obj_ref
                    .extract::<Option<pyo3_chrono::NaiveDateTime>>()?
                    .map(|f| f.0)
                    .to_sql(ty, out),
                Type::TIMESTAMPTZ => obj_ref
                    .extract::<Option<pyo3_chrono::NaiveDateTime>>()?
                    .map(|f| f.0)
                    .to_sql(ty, out),
                Type::BOOL => obj_ref.extract::<Option<bool>>()?.to_sql(ty, out),
                Type::TEXT => {
                    let s = obj_ref.extract::<Option<&str>>();
                    match s {
                        Ok(s) => s.to_sql(ty, out),
                        Err(_) => obj_ref.to_string().to_sql(ty, out),
                    }
                }
                Type::VARCHAR => obj_ref.extract::<Option<&str>>()?.to_sql(ty, out),
                Type::NAME => obj_ref.extract::<Option<&str>>()?.to_sql(ty, out),
                Type::CHAR => obj_ref.extract::<Option<i8>>()?.to_sql(ty, out),
                Type::UNKNOWN => obj_ref.extract::<Option<&str>>()?.to_sql(ty, out),
                Type::INT2 => obj_ref.extract::<Option<i16>>()?.to_sql(ty, out),
                Type::INT4 => obj_ref.extract::<Option<i32>>()?.to_sql(ty, out),
                Type::INT8 => obj_ref.extract::<Option<i64>>()?.to_sql(ty, out),
                Type::FLOAT4 => obj_ref.extract::<Option<f32>>()?.to_sql(ty, out),
                Type::FLOAT8 => obj_ref.extract::<Option<f64>>()?.to_sql(ty, out),
                Type::OID => obj_ref.extract::<Option<u32>>()?.to_sql(ty, out),
                Type::BYTEA => obj_ref.extract::<Option<&[u8]>>()?.to_sql(ty, out),
                Type::BOOL_ARRAY => obj_ref.extract::<Option<Vec<bool>>>()?.to_sql(ty, out),
                Type::TEXT_ARRAY => obj_ref.extract::<Option<Vec<&str>>>()?.to_sql(ty, out),
                Type::VARCHAR_ARRAY => obj_ref.extract::<Option<Vec<&str>>>()?.to_sql(ty, out),
                Type::NAME_ARRAY => obj_ref.extract::<Option<Vec<&str>>>()?.to_sql(ty, out),
                Type::CHAR_ARRAY => obj_ref.extract::<Option<Vec<i8>>>()?.to_sql(ty, out),
                Type::INT2_ARRAY => obj_ref.extract::<Option<Vec<i16>>>()?.to_sql(ty, out),
                Type::INT4_ARRAY => obj_ref.extract::<Option<Vec<i32>>>()?.to_sql(ty, out),
                Type::INT8_ARRAY => obj_ref.extract::<Option<Vec<i64>>>()?.to_sql(ty, out),
                Type::FLOAT4_ARRAY => obj_ref.extract::<Option<Vec<f32>>>()?.to_sql(ty, out),
                Type::FLOAT8_ARRAY => obj_ref.extract::<Option<Vec<f64>>>()?.to_sql(ty, out),
                Type::OID_ARRAY => obj_ref.extract::<Option<Vec<u32>>>()?.to_sql(ty, out),
                Type::BYTEA_ARRAY => obj_ref.extract::<Option<Vec<&[u8]>>>()?.to_sql(ty, out),
                t => {
                    // if let Ok(s) = obj_ref.downcast::<PyString>() {
                    //     return s.to_str()?.to_sql(ty, out);
                    // }
                    // if let Ok(s) = obj_ref.downcast::<PyBytes>() {
                    //     return s.as_bytes().to_sql(ty, out);
                    // }
                    // if let Ok(s) = obj_ref.extract::<i64>() {
                    //     return s.to_sql(ty, out);
                    // }
                    // if let Ok(s) = obj_ref.downcast::<PyFloat>() {
                    //     return s.extract::<f64>()?.to_sql(ty, out);
                    // }
                    Err(anyhow!(
                        "Could not convert postgres type {:?} from python {:?}",
                        t,
                        obj_ref
                    )
                    .into())
                }
            }
        })
    }
    fn accepts(ty: &Type) -> bool
    where
        Self: Sized,
    {
        match *ty {
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
            | Type::BYTEA_ARRAY => true,
            _ => false,
        }
    }

    to_sql_checked!();
}

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
            match db_err.code().clone() {
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

enum LoopResult {
    ChangeAutoCommit(bool),
    Continue,
    Stop,
}

async fn run_loop(
    client_pool: Pool<PostgresConnectionManager<NoTls>>,
    mut rec: Receiver<TxnCommand>,
) -> PuffResult<()> {
    let mut auto_commit = false;
    let mut running = true;
    let mut client_conn = client_pool.get().await?;
    while running {
        if let Some(msg) = rec.recv().await {
            let new_result = if auto_commit {
                run_autocommit_loop(msg, &mut *client_conn, &mut rec).await?
            } else {
                let txn = client_conn.transaction().await?;
                run_txn_loop(msg, txn, &mut rec).await?
            };
            match new_result {
                LoopResult::Continue => running = true,
                LoopResult::Stop => running = false,
                LoopResult::ChangeAutoCommit(new_autocommit) => auto_commit = new_autocommit,
            }
        } else {
            running = false
        }
    }
    Ok(())
}

fn pg_type_to_db2_type(py: Python, ty: &Type) -> PyResult<PyObject> {
    match ty.clone() {
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
        t => get_cached_object(py, "puff.postgres.TypeObject".into())?.call1(py, (t.to_string(),)),
    }
}

fn statement_to_description(statement: &Option<Statement>) -> PyResult<Option<PyObject>> {
    match statement.as_ref() {
        Some(st) => {
            let cols = st.columns();
            if cols.len() == 0 {
                Ok(None)
            } else {
                Python::with_gil(|py| {
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
                        );
                        desc.append(tup)?;
                    }
                    Ok(Some(desc.into_py(py)))
                })
            }
        }
        None => Ok(None),
    }
}

async fn run_txn_loop<'a>(
    first_msg: TxnCommand,
    txn: Transaction<'a>,
    rec: &'a mut Receiver<TxnCommand>,
) -> PuffResult<LoopResult> {
    let mut row_count: i32 = -1;
    let mut current_statement: Option<Statement> = None;
    let mut current_portal: Option<Portal> = None;
    let mut next_message = Some(first_msg);
    let mut is_first = true;
    while let Some(x) = next_message.take() {
        match x {
            TxnCommand::Description(py_obj) => {
                let py_res = statement_to_description(&current_statement);
                handle_python_return(py_obj, ready(py_res)).await;
            }
            TxnCommand::SetAutoCommit(py_obj, new) => {
                handle_python_return(py_obj, ready(Ok(()))).await;
                if new {
                    return Ok(LoopResult::ChangeAutoCommit(new));
                }
            }
            TxnCommand::Close(txn_loop) => {
                txn.rollback().await?;
                let mut m = txn_loop.lock().await;
                *m = None;
                return Ok(LoopResult::Stop);
            }
            TxnCommand::Commit(ret) => {
                if !is_first {
                    let real_return = match txn.commit().await {
                        Ok(_r) => Ok(true),
                        Err(e) => Err(postgres_to_python_exception(e)),
                    };
                    handle_python_return(ret, async { real_return }).await;
                    return Ok(LoopResult::Continue);
                } else {
                    handle_python_return(ret, async { Ok(true) }).await;
                }
            }
            TxnCommand::Rollback(ret) => {
                if !is_first {
                    let real_return = match txn.rollback().await {
                        Ok(_r) => Ok(()),
                        Err(e) => Err(postgres_to_python_exception(e)),
                    };
                    handle_python_return(ret, async { real_return }).await;
                    return Ok(LoopResult::Continue);
                } else {
                    handle_python_return(ret, async { Ok(true) }).await;
                }
            }
            TxnCommand::ExecuteRust(sender, q, params) => {
                current_statement = None;
                row_count = -1;

                let fut = async {
                    let statement = txn.prepare(&q).await?;
                    let rowstream = txn.query_raw(&statement, &params).await?;
                    let rows: Vec<Row> = rowstream.try_collect().await?;
                    Ok((statement, rows))
                };
                sender.send(fut.await).unwrap_or(())
            }
            TxnCommand::Execute(ret, q, params) => {
                current_statement = None;
                row_count = -1;
                is_first = false;

                let stmt = txn.prepare(&q).await;
                let real_return = match stmt {
                    Ok(r) => {
                        current_statement = Some(r.clone());
                        if r.columns().len() == 0 {
                            let res = txn.execute_raw(&r, &params[..]).await;
                            match res {
                                Ok(r) => {
                                    current_portal = None;
                                    row_count = r as i32;
                                    Ok(())
                                }
                                Err(e) => Err(postgres_to_python_exception(e)),
                            }
                        } else {
                            let res = txn.bind_raw(&r, &params[..]).await;
                            match res {
                                Ok(r) => {
                                    current_portal = Some(r);
                                    row_count = -1;
                                    Ok(())
                                }
                                Err(e) => Err(postgres_to_python_exception(e)),
                            }
                        }
                    }
                    Err(e) => Err(postgres_to_python_exception(e)),
                };

                handle_python_return(ret, ready(real_return)).await
            }
            TxnCommand::ExecuteMany(ret, q, param_seq) => {
                current_statement = None;
                let fut = async {
                    let stmt = txn.prepare(&q).await;
                    row_count = -1;

                    match stmt {
                        Ok(r) => {
                            let mut this_row_count = 0;

                            for params in param_seq {
                                let res = txn.execute_raw(&r, &params[..]).await;
                                match res {
                                    Ok(r) => {
                                        current_portal = None;
                                        this_row_count += r as i32;
                                    }
                                    Err(e) => return Err(postgres_to_python_exception(e)),
                                }
                            }

                            row_count = this_row_count;

                            Ok(())
                        }
                        Err(e) => Err(postgres_to_python_exception(e)),
                    }
                };

                handle_python_return(ret, fut).await
            }
            TxnCommand::RowCount(ret) => handle_return(ret, async { Ok(row_count) }).await,
            TxnCommand::FetchOne(ret) => {
                handle_python_return(ret, async {
                    if let Some(v) = current_portal.as_ref() {
                        let result = txn.query_portal(v, 1).await;
                        match result {
                            Ok(vec) => match vec.into_iter().next() {
                                Some(row) => {
                                    Python::with_gil(|py| Ok(row_to_pyton(py, row)?.into_py(py)))
                                }
                                None => {
                                    current_portal = None;
                                    Python::with_gil(|py| Ok(py.None()))
                                }
                            },
                            Err(e) => {
                                current_portal = None;
                                Err(postgres_to_python_exception(e))
                            }
                        }
                    } else {
                        Err(InternalError::new_err(format!(
                            "Fetchone Cursor not ready."
                        )))?
                    }
                })
                .await
            }
            TxnCommand::FetchMany(ret, real_row_count) => {
                handle_python_return::<_, PyObject>(ret, async {
                    if let Some(v) = current_portal.as_ref() {
                        let result = txn.query_portal(v, real_row_count).await;
                        match result {
                            Ok(vec) => Python::with_gil(|py| {
                                let pylist = PyList::empty(py);
                                for v in vec {
                                    pylist.append(row_to_pyton(py, v)?)?;
                                }
                                Ok(pylist.into_py(py))
                            }),
                            Err(e) => {
                                current_portal = None;
                                Err(postgres_to_python_exception(e))
                            }
                        }
                    } else {
                        Err(InternalError::new_err(format!("Cursor not ready.")))?
                    }
                })
                .await
            }
            TxnCommand::FetchAll(ret) => {
                handle_python_return::<_, PyObject>(ret, async {
                    if let Some(v) = current_portal.as_ref() {
                        let result = txn.query_portal(v, -1).await;
                        match result {
                            Ok(vec) => Python::with_gil(|py| {
                                let pylist = PyList::empty(py);
                                for v in vec {
                                    pylist.append(row_to_pyton(py, v)?)?;
                                }
                                Ok(pylist.into_py(py))
                            }),
                            Err(e) => {
                                current_portal = None;
                                Err(postgres_to_python_exception(e))
                            }
                        }
                    } else {
                        Err(InternalError::new_err(format!("Cursor not ready.")))?
                    }
                })
                .await
            }
        }

        next_message = rec.recv().await;
    }
    txn.rollback().await?;
    Ok(LoopResult::Stop)
}

async fn run_autocommit_loop<'a>(
    first_msg: TxnCommand,
    client: &'a mut Client,
    rec: &'a mut Receiver<TxnCommand>,
) -> PuffResult<LoopResult> {
    let mut row_count: i32 = -1;
    let mut current_statement: Option<Statement> = None;
    let mut current_stream: Option<Pin<Box<RowStream>>> = None;
    let mut next_message = Some(first_msg);
    while let Some(x) = next_message.take() {
        match x {
            TxnCommand::Description(py_obj) => {
                let py_res = statement_to_description(&current_statement);
                handle_python_return(py_obj, ready(py_res)).await;
            }
            TxnCommand::SetAutoCommit(ret, new) => {
                handle_python_return(ret, ready(Ok(()))).await;
                if !new {
                    return Ok(LoopResult::ChangeAutoCommit(new));
                }
            }
            TxnCommand::Close(txn_loop) => {
                let mut m = txn_loop.lock().await;
                *m = None;
                return Ok(LoopResult::Stop);
            }
            TxnCommand::Commit(ret) => {
                handle_python_return(ret, async { Ok(true) }).await;
            }
            TxnCommand::Rollback(ret) => {
                handle_python_return(ret, async { Ok(false) }).await;
            }
            TxnCommand::ExecuteRust(sender, q, params) => {
                let fut = async {
                    let statement = client.prepare(&q).await?;
                    let rowstream = client.query_raw(&statement, &params).await?;
                    let rows: Vec<Row> = rowstream.try_collect().await?;
                    Ok((statement, rows))
                };
                sender.send(fut.await).unwrap_or(());
            }
            TxnCommand::Execute(ret, q, params) => {
                row_count = -1;
                current_stream = None;

                let stmt = client.prepare(&q).await;
                let real_return = match stmt {
                    Ok(r) => {
                        current_statement = Some(r.clone());
                        if r.columns().len() == 0 {
                            let res = client.execute_raw(&r, &params[..]).await;
                            match res {
                                Ok(r) => {
                                    row_count = r as i32;
                                    Ok(())
                                }
                                Err(e) => Err(postgres_to_python_exception(e)),
                            }
                        } else {
                            let res = client.query_raw(&r, &params[..]).await;
                            match res {
                                Ok(r) => {
                                    current_stream = Some(Box::pin(r));
                                    row_count = -1;
                                    Ok(())
                                }
                                Err(e) => Err(postgres_to_python_exception(e)),
                            }
                        }
                    }
                    Err(e) => Err(postgres_to_python_exception(e)),
                };

                handle_python_return(ret, async { real_return }).await
            }
            TxnCommand::ExecuteMany(ret, q, param_seq) => {
                current_statement = None;
                let fut = async {
                    let stmt = client.prepare(&q).await;
                    row_count = -1;
                    match stmt {
                        Ok(r) => {
                            let mut this_row_count = 0;
                            for params in param_seq {
                                let res = client.execute_raw(&r, &params[..]).await;
                                match res {
                                    Ok(r) => {
                                        current_stream = None;
                                        this_row_count += r as i32;
                                    }
                                    Err(e) => return Err(postgres_to_python_exception(e)),
                                }

                                row_count = this_row_count;
                            }

                            Ok(())
                        }
                        Err(e) => Err(postgres_to_python_exception(e)),
                    }
                };

                handle_python_return(ret, fut).await
            }
            TxnCommand::RowCount(ret) => handle_return(ret, async { Ok(row_count) }).await,
            TxnCommand::FetchOne(ret) => {
                handle_python_return(ret, async {
                    if let Some(v) = current_stream.as_mut() {
                        let result = v.next().await;
                        match result {
                            Some(Ok(row)) => {
                                Python::with_gil(|py| Ok(row_to_pyton(py, row)?.into_py(py)))
                            }
                            Some(Err(e)) => {
                                current_stream = None;
                                Err(postgres_to_python_exception(e))
                            }
                            None => Python::with_gil(|py| Ok(py.None())),
                        }
                    } else {
                        Err(InternalError::new_err(format!(
                            "Fetchone Cursor not ready."
                        )))?
                    }
                })
                .await
            }
            TxnCommand::FetchMany(ret, real_row_count) => {
                handle_python_return(ret, async {
                    if let Some(v) = current_stream.as_mut() {
                        let num_rows = real_row_count as usize;
                        let mut real_result = Vec::with_capacity(num_rows);
                        for _ in 0..num_rows {
                            let result = v.next().await;
                            match result {
                                Some(Ok(row)) => {
                                    let obj = Python::with_gil(|py| {
                                        PyResult::Ok(row_to_pyton(py, row)?.into_py(py))
                                    })?;
                                    real_result.push(obj);
                                }
                                Some(Err(e)) => {
                                    current_stream = None;
                                    return Err(postgres_to_python_exception(e));
                                }
                                None => {
                                    current_stream = None;
                                    break;
                                }
                            }
                        }
                        Ok(real_result)
                    } else {
                        Ok(Vec::new())
                    }
                })
                .await
            }
            TxnCommand::FetchAll(ret) => {
                handle_python_return(ret, async {
                    if let Some(v) = current_stream.as_mut() {
                        let mut real_result = Vec::new();
                        loop {
                            let result = v.next().await;
                            match result {
                                Some(Ok(row)) => {
                                    let obj = Python::with_gil(|py| {
                                        PyResult::Ok(row_to_pyton(py, row)?.into_py(py))
                                    })?;
                                    real_result.push(obj);
                                }
                                Some(Err(e)) => {
                                    current_stream = None;
                                    return Err(postgres_to_python_exception(e));
                                }
                                None => break,
                            }
                        }
                        Ok(real_result)
                    } else {
                        Err(InternalError::new_err(format!(
                            "Fetchone Cursor not ready."
                        )))?
                    }
                })
                .await
            }
        }

        next_message = rec.recv().await;
    }
    Ok(LoopResult::Stop)
}

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
    puff_pg.add("NotSupportedError", py.get_type::<NotSupportedError>())
}
