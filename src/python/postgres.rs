use crate::context::with_puff_context;
use crate::errors::{handle_puff_error, handle_puff_result, PuffResult};
use crate::python::greenlet::{greenlet_async, handle_return};
use crate::python::postgres::TxnCommand::ExecuteMany;
use anyhow::anyhow;
use bb8_postgres::bb8::Pool;
use bb8_postgres::tokio_postgres::Client;
use bb8_postgres::PostgresConnectionManager;
use futures_util::{FutureExt, StreamExt};
use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyFloat, PyList, PyLong, PyString, PyType};
use pythonize::depythonize;
use std::error::Error;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use chrono::{DateTime, NaiveDateTime, Utc};
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio_postgres::types::private::BytesMut;
use tokio_postgres::types::{to_sql_checked, IsNull, ToSql, Type};
use tokio_postgres::{NoTls, Row, RowStream, Transaction};
use tokio_postgres::error::SqlState;
use tracing::{error, info};
use crate::python::log_traceback_with_label;
use crate::types::Bytes;
use futures_util::Stream;

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
        let pool = with_puff_context(|c| c.postgres());
        Connection::new(pool.pool())
    }
}

#[pyclass]
struct Connection {
    pool: Pool<PostgresConnectionManager<NoTls>>,
    transaction_loop: Arc<Mutex<Option<Sender<TxnCommand>>>>,
}

impl Connection {
    pub fn new(pool: Pool<PostgresConnectionManager<NoTls>>) -> Self {
        Self {
            pool,
            transaction_loop: Arc::new(Mutex::new(None)),
        }
    }
}

#[pymethods]
impl Connection {
    fn close(&mut self, py: Python) {
        py.allow_threads(|| {
            let mut m = self.transaction_loop.lock().unwrap();
            *m = None;
        });
    }
    fn commit(&self, return_fun: PyObject) {
        let ctx = with_puff_context(|ctx| ctx);
        let transaction_loop_inner = self.transaction_loop.clone();
        ctx.handle().spawn(async move {
            let sender = {
                let job_sender = transaction_loop_inner.lock().unwrap();
                job_sender.clone()
            };
            if let Some(s) = sender {
                s.send(TxnCommand::Commit(return_fun)).await
            } else {
                handle_return(return_fun, async { Ok(()) }).await;
                return Ok(());
            }
        });
    }
    fn rollback(&self, return_fun: PyObject) {
        let ctx = with_puff_context(|ctx| ctx);
        let transaction_loop_inner = self.transaction_loop.clone();
        ctx.handle().spawn(async move {
            let sender = {
                let job_sender = transaction_loop_inner.lock().unwrap();
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
            transaction_loop: self.transaction_loop.clone(),
            arraysize: 1,
        };
        cursor.into_py(py)
    }
}

#[pyclass]
struct Cursor {
    pool: Pool<PostgresConnectionManager<NoTls>>,
    transaction_loop: Arc<Mutex<Option<Sender<TxnCommand>>>>,
    arraysize: usize,
}

impl Cursor {
    fn get_sender(&self) -> Sender<TxnCommand> {
        let mut x = self.transaction_loop.lock().unwrap();
        if let Some(l) = x.as_ref() {
            l.clone()
        } else {
            let (sender, rec) = channel(1);
            *x = Some(sender.clone());
            let handle = with_puff_context(|ctx| ctx.handle());
            handle.spawn(run_loop(self.pool.clone(), rec));
            sender
        }
    }

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
                        .map_err(|e| log_traceback_with_label("Postgres return", e))
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
    fn get_arraysize(&self) -> PyResult<usize> {
        Ok(self.arraysize)
    }

    #[setter]
    fn set_arraysize(&mut self, value: usize) -> PyResult<()> {
        self.arraysize = value;
        Ok(())
    }

    fn do_get_rowcount(&self, return_fun: PyObject) {
        let inner_loop = self.transaction_loop.clone();
        self.spawn_and_recover(return_fun.clone(), async move {
            let txn_loop = {
                let m = inner_loop.lock().unwrap();
                m.clone()
            };
            match txn_loop {
                Some(job_sender) => {
                    Ok(job_sender
                        .send(TxnCommand::RowCount(
                            return_fun,
                        ))
                        .await?)
                }
                None => {
                    Python::with_gil(|py| return_fun.call1(py, (py.None(), py.None())))?;
                    Ok(())
                }
            }
        })
    }

    fn setinputsizes(&self, size: PyObject) -> PyResult<()> {
        Ok(())
    }

    fn setoutputsizes(&self, size: PyObject, column: Option<PyObject>) -> PyResult<()> {
        Ok(())
    }

    fn close(&mut self, py: Python) {
        py.allow_threads(|| {
            let mut m = self.transaction_loop.lock().unwrap();
            *m = None;
        });
    }

    fn do_get_query(&self, return_fun: PyObject) {
        let inner_loop = self.transaction_loop.clone();
        self.spawn_and_recover(return_fun.clone(), async move {
            let txn_loop = {
                let m = inner_loop.lock().unwrap();
                m.clone()
            };
            match txn_loop {
                Some(job_sender) => {
                    Ok(job_sender
                        .send(TxnCommand::Query(
                            return_fun,
                        ))
                        .await?)
                }
                None => {
                    Python::with_gil(|py| return_fun.call1(py, (py.None(), py.None())))?;
                    Ok(())
                }
            }

        })
    }

    fn executemany(
        &self,
        py: Python,
        return_func: PyObject,
        operation: String,
        seq_of_parameters: Option<PyObject>,
    ) {
        let job_sender = py.allow_threads(|| self.get_sender());
        self.spawn_and_recover(return_func.clone(), async move {
            Ok(job_sender
                .send(ExecuteMany(
                    return_func,
                    operation,
                    seq_of_parameters
                        .unwrap_or(Python::with_gil(|py| PyList::empty(py).into_py(py))),
                ))
                .await?)
        })
    }

    fn execute(
        &self,
        py: Python,
        return_func: PyObject,
        operation: String,
        parameters: Option<Vec<PyObject>>,
    ) {
        let job_sender = py.allow_threads(|| self.get_sender());
        self.spawn_and_recover(return_func.clone(), async move {
            let p = parameters.unwrap_or_default();
            let sql_p = p.into_iter().map(PythonSqlValue).collect();
            Ok(job_sender
                .send(TxnCommand::Execute(return_func, operation, sql_p))
                .await?)
        });
    }

    fn callproc(&self, procname: &PyString, parameters: Option<&PyList>) {}

    fn fetchone(&mut self, py: Python, return_func: PyObject) {
        let job_sender = py.allow_threads(|| self.get_sender());
        self.spawn_and_recover(return_func.clone(), async move {
            Ok(job_sender.send(TxnCommand::FetchOne(return_func)).await?)
        });
    }

    fn fetchmany(&mut self, py: Python, return_func: PyObject, rowcount: Option<usize>) {
        let real_row_count = rowcount.unwrap_or(self.arraysize);
        let job_sender = py.allow_threads(|| self.get_sender());
        self.spawn_and_recover(return_func.clone(), async move {
            Ok(job_sender
                .send(TxnCommand::FetchMany(return_func, real_row_count))
                .await?)
        });
    }

    fn fetchall(&mut self, py: Python, return_func: PyObject) {
        let job_sender = py.allow_threads(|| self.get_sender());
        self.spawn_and_recover(return_func.clone(), async move {
            Ok(job_sender.send(TxnCommand::FetchAll(return_func)).await?)
        });
    }
}

fn row_to_pyton(py: Python, row: Row) -> PyResult<Py<PyList>> {
    let mut row_vec = Vec::with_capacity(row.len());

    for (ix, c) in row.columns().iter().enumerate() {
        let val = match c.type_().clone() {
            Type::BOOL => row.get::<_, Option<bool>>(ix).into_py(py),
            Type::TIMESTAMP => row.get::<_, Option<NaiveDateTime>>(ix).map(|f| pyo3_chrono::NaiveDateTime::from(f)).into_py(py),
            Type::TIMESTAMPTZ => row.get::<_, Option<DateTime<Utc>>>(ix).map(|f| pyo3_chrono::NaiveDateTime::from(f.naive_local())).into_py(py),
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
            Type::JSON => pythonize::pythonize(py, &row.get::<_, Option<serde_json::Value>>(ix))?,
            Type::JSONB => pythonize::pythonize(py, &row.get::<_, Option<serde_json::Value>>(ix))?,
            Type::BOOL_ARRAY => row.get::<_, Option<Vec<bool>>>(ix).into_py(py),
            Type::TEXT_ARRAY => row.get::<_, Option<Vec<&str>>>(ix).into_py(py),
            Type::VARCHAR_ARRAY => row.get::<_, Option<Vec<&str>>>(ix).into_py(py),
            Type::NAME_ARRAY => row.get::<_, Option<Vec<&str>>>(ix).into_py(py),
            Type::CHAR_ARRAY => row.get::<_, Option<Vec<i8>>>(ix).into_py(py),
            Type::INT2_ARRAY => row.get::<_, Option<Vec<i16>>>(ix).into_py(py),
            Type::INT4_ARRAY => row.get::<_, Option<Vec<i32>>>(ix).into_py(py),
            Type::INT8_ARRAY => row.get::<_, Option<Vec<i64>>>(ix).into_py(py),
            Type::FLOAT4_ARRAY => row.get::<_, Option<Vec<f32>>>(ix).into_py(py),
            Type::FLOAT8_ARRAY => row.get::<_, Option<Vec<f64>>>(ix).into_py(py),
            Type::OID_ARRAY => row.get::<_, Option<Vec<u32>>>(ix).into_py(py),
            Type::BYTEA_ARRAY => row.get::<_, Option<Vec<&[u8]>>>(ix).into_py(py),

            t => {
                return Err(NotSupportedError::new_err(format!(
                    "Unsupported postgres type {:?}",
                    t
                )))
            }
        };
        row_vec.push(val)
    }
    Ok(PyList::new(py, row_vec).into_py(py))
}

#[derive(Debug)]
enum TxnCommand {
    Execute(PyObject, String, Vec<PythonSqlValue>),
    ExecuteMany(PyObject, String, PyObject),
    FetchOne(PyObject),
    FetchMany(PyObject, usize),
    FetchAll(PyObject),
    Commit(PyObject),
    Rollback(PyObject),
    Query(PyObject),
    RowCount(PyObject),
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
                Type::TIMESTAMP => obj_ref.extract::<Option<pyo3_chrono::NaiveDateTime>>()?.map(|f| f.0).to_sql(ty, out),
                Type::TIMESTAMPTZ => obj_ref.extract::<Option<pyo3_chrono::NaiveDateTime>>()?.map(|f| f.0).to_sql(ty, out),
                Type::BOOL => obj_ref.extract::<Option<bool>>()?.to_sql(ty, out),
                Type::TEXT => {
                    let s = obj_ref.extract::<Option<&str>>();
                    match s {
                        Ok(s) => s.to_sql(ty, out),
                        Err(_) => obj_ref.to_string().to_sql(ty, out),
                    }
                },
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
            | Type::BYTEA_ARRAY => true,
            _ => false,
        }
    }

    to_sql_checked!();
}

async fn run_loop(
    client_pool: Pool<PostgresConnectionManager<NoTls>>,
    mut rec: Receiver<TxnCommand>,
) -> PuffResult<()> {
    let mut running = true;
    while running {
        let mut client_conn = client_pool.get().await?;
        let txn = client_conn.transaction().await?;
        running = run_txn_loop(txn, &mut rec).await?
    }
    Ok(())
}

fn postgres_to_python_exception(e: tokio_postgres::Error) -> PyErr {
    if e.is_closed() {
        return InternalError::new_err(format!(
                "Error occurred because connection is closed: {:?}",
                e
            ))
    }
    match e.as_db_error() {
        Some(db_err) => {
            let message = db_err.message();
            let hint = db_err.hint().map(|f| format!(" Hint: {}", f)).unwrap_or_default();
            let severity = db_err.severity();
            let pos = db_err.hint().map(|f| format!(" Position: {}", f)).unwrap_or_default();
            let schema = db_err.schema().map(|f| format!(" Schema: {}", f)).unwrap_or_default();
            let table = db_err.table().map(|f| format!(" Table: {}", f)).unwrap_or_default();
            let column = db_err.column().map(|f| format!(" Column: {}", f)).unwrap_or_default();
            let constraint = db_err.constraint().map(|f| format!(" Constraint: {}", f)).unwrap_or_default();
            let formatted = format!("Message: {}, Severity: {}{}{}{}{}{}{}", message, severity, hint, pos, schema, table, column, constraint);
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
                SqlState::INVALID_ARGUMENT_FOR_NTH_VALUE=> DataError::new_err(formatted),
                SqlState::INVALID_ARGUMENT_FOR_SQL_JSON_DATETIME_FUNCTION=> DataError::new_err(formatted),
                SqlState::INVALID_ARGUMENT_FOR_NTILE=> DataError::new_err(formatted),
                SqlState::INVALID_ARGUMENT_FOR_POWER_FUNCTION=> DataError::new_err(formatted),
                SqlState::INVALID_ARGUMENT_FOR_WIDTH_BUCKET_FUNCTION => DataError::new_err(formatted),
                SqlState::NUMERIC_VALUE_OUT_OF_RANGE => DataError::new_err(formatted),
                SqlState::DATETIME_FIELD_OVERFLOW => DataError::new_err(formatted),
                SqlState::DATETIME_VALUE_OUT_OF_RANGE => DataError::new_err(formatted),
                SqlState::NULL_VALUE_NOT_ALLOWED => DataError::new_err(formatted),
                _ => InternalError::new_err(formatted),
            }
        }
        None => {
            InternalError::new_err(format!(
                "{:?}",
                e
            ))
        }
    }
}


async fn run_txn_loop<'a>(
    mut txn: Transaction<'a>,
    mut rec: &'a mut Receiver<TxnCommand>,
) -> PuffResult<bool> {
    let mut query: Option<String> = None;
    let mut current_stream: Option<Pin<Box<RowStream>>> = None;
    while let Some(x) = rec.recv().await {
        println!("{:?}", x);
        match x {
            TxnCommand::Query(ret) => {
                handle_return(ret, async { Ok(query.as_ref().map(|f| Bytes::copy_from_slice(f.as_bytes()))) }).await;
            }
            TxnCommand::Commit(ret) => {
                let real_return = match txn.commit().await {
                    Ok(r) => Ok(()),
                    Err(e) => {
                        Err(postgres_to_python_exception(e))
                    }
                };
                handle_return(ret, async { Ok(real_return?) }).await;
                return Ok(true);
            }
            TxnCommand::Rollback(ret) => {
                let real_return = match txn.rollback().await {
                    Ok(r) => Ok(()),
                    Err(e) => {
                        Err(postgres_to_python_exception(e))
                    }
                };
                handle_return(ret, async { Ok(real_return?) }).await;
                return Ok(true);
            }
            TxnCommand::Execute(ret, q, params) => {

                let res = txn.query_raw(&q, &params[..]).await;
                let real_return = match res {
                    Ok(r) => {
                        query = Some(q);
                        current_stream = Some(Box::pin(r));
                        Ok(())
                    }
                    Err(e) => {
                        Err(postgres_to_python_exception(e))
                    }
                };

                handle_return(ret, async { Ok(real_return?) }).await
            }
            TxnCommand::ExecuteMany(ret, q, param_seq) => {
                handle_return(ret, async {
                    let param_groups = Python::with_gil(|py| {
                        let mut v = Vec::new();
                        for params in param_seq.into_ref(py).iter()? {
                            v.push(params?.into_py(py))
                        }
                        PuffResult::Ok(v)
                    })?;
                    let mut v = Vec::new();
                    for params in param_groups {
                        let ps: Vec<PythonSqlValue> = Python::with_gil(|py| {
                            PuffResult::Ok(
                                params
                                    .extract::<Vec<PyObject>>(py)?
                                    .into_iter()
                                    .map(PythonSqlValue)
                                    .collect(),
                            )
                        })?;

                        let res = txn.execute_raw(&q, ps).await?;
                        v.push(res)
                    }
                    PuffResult::Ok(v)
                })
                .await
            }
            TxnCommand::RowCount(ret) => {
                handle_return(ret, async {
                    if let Some(mut v) = current_stream.as_mut() {
                        let (lower, upper) = v.size_hint();
                        println!("size {:?} {:?}", lower, upper);
                        Ok(upper.unwrap_or_default() as i32)
                    } else {
                        Ok(-1)
                    }
                })
                .await
            }
            TxnCommand::FetchOne(ret) => {
                handle_return(ret, async {
                    if let Some(mut v) = current_stream.as_mut() {
                        let next_one = v.next().await;
                        match next_one {
                            Some(Ok(r)) => {
                                Python::with_gil(|py| Ok(row_to_pyton(py, r)?.into_py(py)))
                            }
                            Some(Err(e)) => Err(postgres_to_python_exception(e))?,
                            None => Ok(Python::with_gil(|py| py.None())).into(),
                        }
                    } else {
                        Err(InternalError::new_err(format!("Cursor not ready.")))?
                    }
                })
                .await
            }
            TxnCommand::FetchMany(ret, real_row_count) => {
                handle_return(ret, async {
                    let mut real_result = Vec::with_capacity(real_row_count);
                    if let Some(mut v) = current_stream.as_mut() {
                        for _ in 0..real_row_count {
                            if let Some(next_one) = v.next().await {
                                let value = match next_one {
                                    Ok(r) => {
                                        let new_r = Python::with_gil(|py| row_to_pyton(py, r))?;
                                        new_r
                                    }
                                    Err(e) => Err(postgres_to_python_exception(e))?,
                                };
                                real_result.push(value)
                            } else {
                                query = None;
                                current_stream = None;
                                break;
                            }
                        }
                    }
                    Python::with_gil(|py| Ok(real_result.into_py(py)))
                })
                .await
            }
            TxnCommand::FetchAll(ret) => {
                handle_return(ret, async {
                    let mut real_result = Vec::new();
                    if let Some(mut v) = current_stream.as_mut() {
                        while let Some(next_one) = v.next().await {
                            let value = match next_one {
                                Ok(r) => {
                                    let new_r = Python::with_gil(|py| row_to_pyton(py, r))?;
                                    new_r
                                }
                                Err(e) => Err(postgres_to_python_exception(e))?,
                            };
                            real_result.push(value)
                        }
                    }
                    query = None;
                    current_stream = None;
                    Python::with_gil(|py| Ok(real_result.into_py(py)))
                })
                .await
            }
        }
    }
    Ok(false)
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
