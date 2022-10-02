use crate::context::with_puff_context;
use crate::errors::PuffResult;
use crate::python::greenlet::{greenlet_async, handle_return};
use crate::python::postgres::TxnCommand::ExecuteMany;
use anyhow::anyhow;
use bb8_postgres::tokio_postgres::Client;
use futures_util::{FutureExt, StreamExt};
use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyFloat, PyList, PyLong, PyString, PyType};
use pythonize::depythonize;
use std::error::Error;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use bb8_postgres::bb8::Pool;
use bb8_postgres::PostgresConnectionManager;
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio_postgres::types::private::BytesMut;
use tokio_postgres::types::{to_sql_checked, IsNull, ToSql, Type};
use tokio_postgres::{NoTls, Row, RowStream, Transaction};
use tracing::{error, info};

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
            transaction_loop: Arc::new(Mutex::new(None))
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
    fn commit(&self) {}
    fn rollback(&self) {}
    fn cursor(&self, py: Python) -> PyObject {
        let cursor = Cursor {
            pool: self.pool.clone(),
            transaction_loop: self.transaction_loop.clone(),
            arraysize: 1
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

    #[getter]
    fn get_rowcount(&self) -> PyResult<i32> {
        Ok(-1)
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

    fn executemany(
        &self,
        return_func: PyObject,
        operation: String,
        seq_of_parameters: Option<PyObject>,
    ) {
        let ctx = with_puff_context(|ctx| ctx);
        let job_sender = self.get_sender();
        ctx.handle().spawn(async move {
            job_sender
                .send(ExecuteMany(
                    return_func,
                    operation,
                    seq_of_parameters
                        .unwrap_or(Python::with_gil(|py| PyList::empty(py).into_py(py))),
                ))
                .await
        });
    }

    fn execute(&self, return_func: PyObject, operation: String, parameters: Option<Vec<PyObject>>) {
        let ctx = with_puff_context(|ctx| ctx);
        let job_sender = self.get_sender();
        ctx.handle().spawn(async move {
            let p = parameters.unwrap_or_default();
            let sql_p = p.into_iter().map(PythonSqlValue).collect();
            job_sender
                .send(TxnCommand::Execute(return_func, operation, sql_p))
                .await
        });
    }

    fn callproc(&self, procname: &PyString, parameters: Option<&PyList>) {}

    fn fetchone(&mut self, return_func: PyObject) {
        let ctx = with_puff_context(|ctx| ctx);
        let job_sender = self.get_sender();
        ctx.handle()
            .spawn(async move {
                if job_sender.send(TxnCommand::FetchOne(return_func)).await.is_err() {
                    error!("Could not send fetchone.")
                } });
    }

    fn fetchmany(&mut self, return_func: PyObject, rowcount: Option<usize>) {
        let real_row_count = rowcount.unwrap_or(self.arraysize);
        let ctx = with_puff_context(|ctx| ctx);
        let job_sender = self.get_sender();
        ctx.handle().spawn(async move {
            if job_sender
                .send(TxnCommand::FetchMany(return_func, real_row_count))
                .await.is_err() {
                error!("Could not send fetchmany.")
            }
        });
    }

    fn fetchall(&mut self, return_func: PyObject) {
        let ctx = with_puff_context(|ctx| ctx);
        let job_sender = self.get_sender();
        ctx.handle()
            .spawn(async move { if job_sender.send(TxnCommand::FetchAll(return_func)).await.is_err() {
                error!("Could not send fetchall.")
            } });
    }
}

fn row_to_pyton(py: Python, row: Row) -> PyResult<Py<PyList>> {
    let mut row_vec = Vec::with_capacity(row.len());

    for (ix, c) in row.columns().iter().enumerate() {
        let val = match c.type_().clone() {
            Type::BOOL => row.get::<_, bool>(ix).into_py(py),
            Type::TEXT => row.get::<_, &str>(ix).into_py(py),
            Type::VARCHAR => row.get::<_, &str>(ix).into_py(py),
            Type::NAME => row.get::<_, &str>(ix).into_py(py),
            Type::CHAR => row.get::<_, i8>(ix).into_py(py),
            Type::UNKNOWN => row.get::<_, &str>(ix).into_py(py),
            Type::INT2 => row.get::<_, i16>(ix).into_py(py),
            Type::INT4 => row.get::<_, i32>(ix).into_py(py),
            Type::INT8 => row.get::<_, i64>(ix).into_py(py),
            Type::FLOAT4 => row.get::<_, f32>(ix).into_py(py),
            Type::FLOAT8 => row.get::<_, f64>(ix).into_py(py),
            Type::OID => row.get::<_, u32>(ix).into_py(py),
            Type::BYTEA => row.get::<_, &[u8]>(ix).into_py(py),
            Type::JSON => pythonize::pythonize(py, &row.get::<_, serde_json::Value>(ix))?,
            Type::BOOL_ARRAY => row.get::<_, Vec<bool>>(ix).into_py(py),
            Type::TEXT_ARRAY => row.get::<_, Vec<&str>>(ix).into_py(py),
            Type::VARCHAR_ARRAY => row.get::<_, Vec<&str>>(ix).into_py(py),
            Type::NAME_ARRAY => row.get::<_, Vec<&str>>(ix).into_py(py),
            Type::CHAR_ARRAY => row.get::<_, Vec<i8>>(ix).into_py(py),
            Type::INT2_ARRAY => row.get::<_, Vec<i16>>(ix).into_py(py),
            Type::INT4_ARRAY => row.get::<_, Vec<i32>>(ix).into_py(py),
            Type::INT8_ARRAY => row.get::<_, Vec<i64>>(ix).into_py(py),
            Type::FLOAT4_ARRAY => row.get::<_, Vec<f32>>(ix).into_py(py),
            Type::FLOAT8_ARRAY => row.get::<_, Vec<f64>>(ix).into_py(py),
            Type::OID_ARRAY => row.get::<_, Vec<u32>>(ix).into_py(py),
            Type::BYTEA_ARRAY => row.get::<_, Vec<&[u8]>>(ix).into_py(py),

            t => {
                return Err(PyException::new_err(format!(
                    "Unsupported postgres type {:?}",
                    t
                )))
            }
        };
        row_vec.push(val)
    }
    Ok(PyList::new(py, row_vec).into_py(py))
}

enum TxnCommand {
    Execute(PyObject, String, Vec<PythonSqlValue>),
    ExecuteMany(PyObject, String, PyObject),
    FetchOne(PyObject),
    FetchMany(PyObject, usize),
    FetchAll(PyObject),
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
                Type::JSON => depythonize::<serde_json::Value>(obj_ref)?.to_sql(ty, out),
                Type::JSONB => depythonize::<serde_json::Value>(obj_ref)?.to_sql(ty, out),
                Type::BOOL => obj_ref.extract::<bool>()?.to_sql(ty, out),
                Type::TEXT => obj_ref.extract::<&str>()?.to_sql(ty, out),
                Type::VARCHAR => obj_ref.extract::<&str>()?.to_sql(ty, out),
                Type::NAME => obj_ref.extract::<&str>()?.to_sql(ty, out),
                Type::CHAR => obj_ref.extract::<i8>()?.to_sql(ty, out),
                Type::UNKNOWN => obj_ref.extract::<&str>()?.to_sql(ty, out),
                Type::INT2 => obj_ref.extract::<i16>()?.to_sql(ty, out),
                Type::INT4 => obj_ref.extract::<i32>()?.to_sql(ty, out),
                Type::INT8 => obj_ref.extract::<i64>()?.to_sql(ty, out),
                Type::FLOAT4 => obj_ref.extract::<f32>()?.to_sql(ty, out),
                Type::FLOAT8 => obj_ref.extract::<f64>()?.to_sql(ty, out),
                Type::OID => obj_ref.extract::<u32>()?.to_sql(ty, out),
                Type::BYTEA => obj_ref.extract::<&[u8]>()?.to_sql(ty, out),
                Type::BOOL_ARRAY => obj_ref.extract::<Vec<bool>>()?.to_sql(ty, out),
                Type::TEXT_ARRAY => obj_ref.extract::<Vec<&str>>()?.to_sql(ty, out),
                Type::VARCHAR_ARRAY => obj_ref.extract::<Vec<&str>>()?.to_sql(ty, out),
                Type::NAME_ARRAY => obj_ref.extract::<Vec<&str>>()?.to_sql(ty, out),
                Type::CHAR_ARRAY => obj_ref.extract::<Vec<i8>>()?.to_sql(ty, out),
                Type::INT2_ARRAY => obj_ref.extract::<Vec<i16>>()?.to_sql(ty, out),
                Type::INT4_ARRAY => obj_ref.extract::<Vec<i32>>()?.to_sql(ty, out),
                Type::INT8_ARRAY => obj_ref.extract::<Vec<i64>>()?.to_sql(ty, out),
                Type::FLOAT4_ARRAY => obj_ref.extract::<Vec<f32>>()?.to_sql(ty, out),
                Type::FLOAT8_ARRAY => obj_ref.extract::<Vec<f64>>()?.to_sql(ty, out),
                Type::OID_ARRAY => obj_ref.extract::<Vec<u32>>()?.to_sql(ty, out),
                Type::BYTEA_ARRAY => obj_ref.extract::<Vec<&[u8]>>()?.to_sql(ty, out),
                t => {
                    if let Ok(s) = obj_ref.downcast::<PyString>() {
                        return s.to_str()?.to_sql(ty, out);
                    }
                    if let Ok(s) = obj_ref.downcast::<PyBytes>() {
                        return s.as_bytes().to_sql(ty, out);
                    }
                    if let Ok(s) = obj_ref.downcast::<PyLong>() {
                        return s.extract::<i64>()?.to_sql(ty, out);
                    }
                    if let Ok(s) = obj_ref.downcast::<PyFloat>() {
                        return s.extract::<f64>()?.to_sql(ty, out);
                    }
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

async fn run_loop(client_pool: Pool<PostgresConnectionManager<NoTls>>, mut rec: Receiver<TxnCommand>) -> PuffResult<()> {
    let mut running = true;
    while running {
        let mut client_conn = client_pool.get().await?;
        let txn = client_conn.transaction().await?;
        running = run_txn_loop(txn, &mut rec).await?
    }
    Ok(())
}


async fn run_txn_loop<'a>(mut txn: Transaction<'a>, mut rec: &'a mut Receiver<TxnCommand>) -> PuffResult<bool> {
    let mut current_stream: Option<Pin<Box<RowStream>>> = None;
    while let Some(x) = rec.recv().await {
        match x {
            TxnCommand::Execute(ret, q, params) => {
                let res = txn.query_raw(&q, &params[..]).await?;
                current_stream = Some(Box::pin(res));
                handle_return(ret, async { Ok(0) }).await
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
                            let ps: Vec<PythonSqlValue> = Python::with_gil(|py| PuffResult::Ok(params.extract::<Vec<PyObject>>(py)?.into_iter().map(PythonSqlValue).collect()))?;

                            let res = txn.execute_raw(&q, ps).await?;
                            v.push(res)
                        }
                        PuffResult::Ok(v)
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
                            Some(Err(e)) => Err(PyException::new_err(format!(
                                "Error getting postgres row {:?}",
                                e
                            )))?,
                            None => Ok(Python::with_gil(|py| py.None())).into(),
                        }
                    } else {
                        Err(PyException::new_err(format!("Cursor not ready.")))?
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
                                    Err(e) => Err(PyException::new_err(format!(
                                        "Error getting postgres row {:?}",
                                        e
                                    )))?,
                                };
                                real_result.push(value)
                            } else {
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
                                Err(e) => Err(PyException::new_err(format!(
                                    "Error getting postgres row {:?}",
                                    e
                                )))?,
                            };
                            real_result.push(value)
                        }
                    }
                    Python::with_gil(|py| Ok(real_result.into_py(py)))
                })
                .await
            }
        }
    }
    Ok(false)
}
