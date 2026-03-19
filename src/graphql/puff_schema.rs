//! Core schema types and resolver logic for async-graphql dynamic schema.
//!
//! Retains the Python dataclass parsing (AggroObject, AggroField, etc.) and
//! the layer-based resolution strategy, but builds async_graphql::dynamic types
//! instead of juniper types.

use crate::context::with_puff_context;
use crate::errors::{to_py_error, PuffResult};
use crate::graphql::row_return::{ExtractValues, PostgresResultRows, PythonResultRows};
use crate::graphql::scalar::AggroValue;
use crate::python::async_python::run_python_async;
use crate::python::postgres::{
    execute_rust, execute_rust_native, gql_value_to_rust_sql, Connection, PythonSqlValue,
    RustSqlValue,
};
use crate::types::text::ToText;
use crate::types::Text;
use anyhow::{anyhow, bail};

use async_graphql::dynamic::*;
use async_graphql::Value as GqlValue;

use pyo3::prelude::*;
use pyo3::types::{IntoPyDict, PyBytes, PyDict, PyList, PyString, PyTuple};
use std::collections::{BTreeMap, HashSet};

use base64::Engine;
use chrono::{DateTime, NaiveDateTime};
use futures_util::future::join_all;
use futures_util::FutureExt;
use indexmap::IndexMap;
use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::Mutex;

use crate::graphql::PuffGraphqlConfig;
use uuid::Uuid;

static NUMBERS: &[&str] = &["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];

pub(crate) fn args_to_py_dict<'py>(
    py: Python<'py>,
    args: &BTreeMap<Text, PyObject>,
) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    for (k, v) in args.iter() {
        dict.set_item(k.as_str(), v)?;
    }
    Ok(dict)
}

// ---------------------------------------------------------------------------
// AggroContext -- request-scoped context passed through async-graphql Data
// ---------------------------------------------------------------------------

pub struct AggroContext {
    auth: Option<PyObject>,
    conn: Option<Mutex<Connection>>,
    config: PuffGraphqlConfig,
}

impl AggroContext {
    pub fn new(auth: Option<PyObject>, config: PuffGraphqlConfig) -> Self {
        Self {
            auth,
            config,
            conn: None,
        }
    }

    pub fn new_with_connection(
        auth: Option<PyObject>,
        conn: Option<Connection>,
        config: PuffGraphqlConfig,
    ) -> Self {
        let conn = conn.map(Mutex::new);
        Self { auth, config, conn }
    }

    pub fn connection(&self) -> Option<&Mutex<Connection>> {
        self.conn.as_ref()
    }

    pub fn config(&self) -> &PuffGraphqlConfig {
        &self.config
    }

    pub fn maybe_connection(&self) -> &Option<Mutex<Connection>> {
        &self.conn
    }

    pub fn auth(&self) -> Option<PyObject> {
        Python::with_gil(|py| self.auth.as_ref().map(|o| o.clone_ref(py)))
    }
}

// ---------------------------------------------------------------------------
// Schema type definitions (kept from original)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum AggroTypeInfo {
    String,
    Int,
    Boolean,
    Datetime,
    Uuid,
    Binary,
    Float,
    Any,
    List(Box<DecodedType>),
    Object(Text),
}

impl AggroTypeInfo {
    fn is_list(&self) -> bool {
        matches!(self, AggroTypeInfo::List(_))
    }
}

pub(crate) fn supports_direct_value_fast_path(type_info: &AggroTypeInfo) -> bool {
    match type_info {
        AggroTypeInfo::String
        | AggroTypeInfo::Int
        | AggroTypeInfo::Boolean
        | AggroTypeInfo::Float
        | AggroTypeInfo::Any => true,
        AggroTypeInfo::List(inner) => supports_direct_value_fast_path(&inner.type_info),
        AggroTypeInfo::Datetime
        | AggroTypeInfo::Uuid
        | AggroTypeInfo::Binary
        | AggroTypeInfo::Object(_) => false,
    }
}

#[derive(Debug, Clone)]
pub struct DecodedType {
    pub optional: bool,
    pub type_info: AggroTypeInfo,
}

#[derive(Debug)]
pub struct AggroArgument {
    pub default: Py<PyAny>,
    pub param_type: DecodedType,
}

impl Clone for AggroArgument {
    fn clone(&self) -> Self {
        Python::with_gil(|py| Self {
            default: self.default.clone_ref(py),
            param_type: self.param_type.clone(),
        })
    }
}

#[derive(Debug)]
pub struct AggroField {
    pub name: Text,
    pub return_type: DecodedType,
    pub is_async: bool,
    pub depends_on: Vec<Text>,
    pub value_from_column: Option<Text>,
    pub producer_method: Option<Py<PyAny>>,
    pub acceptor_method: Option<Py<PyAny>>,
    pub arguments: BTreeMap<Text, AggroArgument>,
    pub safe_without_context: bool,
    pub default: Py<PyAny>,
    /// Static SQL query -- if set, bypasses Python producer entirely (zero-Python fast path)
    pub sql_template: Option<String>,
    /// Maps GraphQL argument names to SQL parameter positions ($1, $2, ...)
    pub sql_arg_names: Vec<String>,
    /// Parent correlation key columns (for child fields)
    pub sql_parent_keys: Vec<Text>,
    /// Child correlation key columns (for child fields)
    pub sql_child_keys: Vec<Text>,
}

impl Clone for AggroField {
    fn clone(&self) -> Self {
        Python::with_gil(|py| Self {
            name: self.name.clone(),
            return_type: self.return_type.clone(),
            is_async: self.is_async,
            depends_on: self.depends_on.clone(),
            value_from_column: self.value_from_column.clone(),
            producer_method: self.producer_method.as_ref().map(|o| o.clone_ref(py)),
            acceptor_method: self.acceptor_method.as_ref().map(|o| o.clone_ref(py)),
            arguments: self.arguments.clone(),
            safe_without_context: self.safe_without_context,
            default: self.default.clone_ref(py),
            sql_template: self.sql_template.clone(),
            sql_arg_names: self.sql_arg_names.clone(),
            sql_parent_keys: self.sql_parent_keys.clone(),
            sql_child_keys: self.sql_child_keys.clone(),
        })
    }
}

impl AggroField {
    pub fn as_argument(&self) -> AggroArgument {
        Python::with_gil(|py| AggroArgument {
            default: self.default.clone_ref(py),
            param_type: self.return_type.clone(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct AggroObject {
    pub name: Text,
    pub fields: Arc<BTreeMap<Text, AggroField>>,
}

fn decode_type(t: &Bound<'_, PyAny>) -> PyResult<DecodedType> {
    let optional: bool = t.getattr("optional")?.extract()?;
    let type_info_str: Text = t.getattr("type_info")?.extract()?;
    let type_info = match type_info_str.as_str() {
        "List" => {
            let inner_type_python: Bound<'_, PyAny> = t.getattr("inner_type")?;
            AggroTypeInfo::List(Box::new(decode_type(&inner_type_python)?))
        }
        "String" => AggroTypeInfo::String,
        "Binary" => AggroTypeInfo::Binary,
        "Datetime" => AggroTypeInfo::Datetime,
        "Uuid" => AggroTypeInfo::Uuid,
        "Any" => AggroTypeInfo::Any,
        "Int" => AggroTypeInfo::Int,
        "Float" => AggroTypeInfo::Float,
        "Boolean" => AggroTypeInfo::Boolean,
        _t => AggroTypeInfo::Object(type_info_str),
    };

    Ok(DecodedType {
        optional,
        type_info,
    })
}

#[allow(clippy::type_complexity)]
pub fn convert(
    py: Python,
    schema: &Bound<'_, PyAny>,
    type_to_description: &Bound<'_, PyAny>,
) -> PyResult<(
    Option<PyObject>,
    bool,
    BTreeMap<Text, AggroObject>,
    BTreeMap<Text, AggroObject>,
)> {
    let description = type_to_description.call1((schema,))?;
    let all_types = description
        .getattr("all_types")?
        .downcast_into::<PyDict>()?;
    let input_types = description
        .getattr("input_types")?
        .downcast_into::<PyDict>()?;
    let py_auth_function = description.getattr("auth_function")?;
    let py_auth_async: bool = description.getattr("auth_async")?.extract()?;
    let auth_function = if !py_auth_function.is_none() {
        Some(py_auth_function.into_py(py))
    } else {
        None
    };

    let mut return_objs = BTreeMap::new();
    for (k, v) in all_types.iter() {
        let s = k.to_string();
        let obj = convert_obj(s.as_str(), v.extract()?)?;
        return_objs.insert(s.to_text(), obj);
    }

    let mut return_inputs = BTreeMap::new();
    for (k, v) in input_types.iter() {
        let s = k.to_string();
        let obj = convert_obj(s.as_str(), v.extract()?)?;
        return_inputs.insert(s.to_text(), obj);
    }
    Ok((auth_function, py_auth_async, return_objs, return_inputs))
}

pub fn convert_obj(name: &str, desc: BTreeMap<String, Bound<'_, PyAny>>) -> PyResult<AggroObject> {
    let mut object_fields: BTreeMap<Text, AggroField> = BTreeMap::new();

    for (k, field_description) in desc.iter() {
        let args = field_description
            .getattr("arguments")?
            .downcast_into::<PyDict>()?;
        let mut final_arguments = BTreeMap::new();

        for (param_name, param) in args.iter() {
            let arg = AggroArgument {
                param_type: decode_type(&param.getattr("param_type")?)?,
                default: param.getattr("default")?.extract()?,
            };
            final_arguments.insert(param_name.str()?.to_text(), arg);
        }

        let depends_on = field_description
            .getattr("depends_on")?
            .extract::<Option<_>>()?
            .unwrap_or_default();

        // Detect @sql decorator attributes on the producer method
        let producer: Option<Py<PyAny>> = field_description.getattr("producer")?.extract()?;
        let (sql_template, sql_arg_names, sql_parent_keys, sql_child_keys) =
            if let Some(ref prod) = producer {
                let py = field_description.py();
                let py_producer = prod.bind(py);
                let sql = py_producer.getattr("__puff_sql__").ok().and_then(|v| {
                    if v.is_none() {
                        None
                    } else {
                        v.extract::<String>().ok()
                    }
                });
                let args = py_producer
                    .getattr("__puff_sql_args__")
                    .ok()
                    .and_then(|v| v.extract::<Vec<String>>().ok())
                    .unwrap_or_default();
                let parent_keys = py_producer
                    .getattr("__puff_sql_parent_keys__")
                    .ok()
                    .and_then(|v| v.extract::<Vec<Text>>().ok())
                    .unwrap_or_default();
                let child_keys = py_producer
                    .getattr("__puff_sql_child_keys__")
                    .ok()
                    .and_then(|v| v.extract::<Vec<Text>>().ok())
                    .unwrap_or_default();
                (sql, args, parent_keys, child_keys)
            } else {
                (None, vec![], vec![], vec![])
            };

        let field = AggroField {
            depends_on,
            name: k.into(),
            return_type: decode_type(&field_description.getattr("return_type")?)?,
            is_async: field_description.getattr("is_async")?.extract()?,
            producer_method: producer,
            acceptor_method: field_description.getattr("acceptor")?.extract()?,
            value_from_column: field_description.getattr("value_from_column")?.extract()?,
            safe_without_context: field_description
                .getattr("safe_without_context")?
                .extract()?,
            default: field_description.getattr("default")?.extract()?,
            arguments: final_arguments,
            sql_template,
            sql_arg_names,
            sql_parent_keys,
            sql_child_keys,
        };

        object_fields.insert(k.into(), field);
    }
    Ok(AggroObject {
        name: name.to_text(),
        fields: Arc::new(object_fields),
    })
}

// ---------------------------------------------------------------------------
// PyContext (passed to Python producer/acceptor methods)
// ---------------------------------------------------------------------------

#[pyclass]
pub struct PyContext {
    conn: Option<Connection>,
    required_columns: Vec<Text>,
    extractor: Arc<dyn ExtractValues + Send + Sync>,
    auth: Option<PyObject>,
    cache: Py<PyDict>,
}

impl Clone for PyContext {
    fn clone(&self) -> Self {
        Python::with_gil(|py| Self {
            conn: self.conn.clone(),
            required_columns: self.required_columns.clone(),
            extractor: self.extractor.clone(),
            auth: self.auth.as_ref().map(|o| o.clone_ref(py)),
            cache: self.cache.clone_ref(py),
        })
    }
}

impl PyContext {
    pub fn new(
        extractor: Arc<dyn ExtractValues + Send + Sync>,
        auth: Option<PyObject>,
        conn: Option<Connection>,
        cache: Py<PyDict>,
        required_columns: Vec<Text>,
    ) -> Self {
        Self {
            extractor,
            auth,
            conn,
            cache,
            required_columns,
        }
    }
}

#[pymethods]
impl PyContext {
    fn parent_values(&self, py: Python, names: Vec<Bound<'_, PyString>>) -> PyResult<PyObject> {
        let rows = to_py_error("Gql Extract", self.extractor.extract_py_values(py, &names))?;
        Ok(rows)
    }

    fn auth(&self, py: Python) -> Option<PyObject> {
        self.auth.as_ref().map(|o| o.clone_ref(py))
    }

    fn layer_cache<'a>(&'a self, py: Python<'a>) -> Bound<'a, PyDict> {
        self.cache.bind(py).clone()
    }

    fn connection(&self, py: Python) -> Option<PyObject> {
        self.conn.clone().map(|c| c.into_py(py))
    }

    fn required_columns(&self, py: Python) -> PyResult<PyObject> {
        let list = PyList::new(py, self.required_columns.iter().map(|t| t.as_str()))?;
        Ok(list.into_py(py))
    }
}

// ---------------------------------------------------------------------------
// Input conversion (Python arguments from async_graphql::Value)
// ---------------------------------------------------------------------------

fn input_value_to_python(
    py: Python,
    t: &DecodedType,
    all_inputs: &Arc<BTreeMap<Text, AggroObject>>,
    v: &GqlValue,
) -> PuffResult<Py<PyAny>> {
    if v == &GqlValue::Null {
        if t.optional {
            return Ok(py.None());
        } else {
            bail!("Null supplied to non-optional field");
        }
    }

    match &t.type_info {
        AggroTypeInfo::List(inner_t) => match v {
            GqlValue::List(inner) => {
                let mut val_vec: Vec<PyObject> = Vec::with_capacity(inner.len());
                for iv in inner {
                    val_vec.push(input_value_to_python(py, inner_t, all_inputs, iv)?);
                }
                PuffResult::Ok(PyList::new(py, val_vec)?.into_py(py))
            }
            _ => bail!("Input non-list to a list input"),
        },
        AggroTypeInfo::Object(inner_t_name) => match v {
            GqlValue::Object(inner) => {
                if let Some(inner_t) = all_inputs.get(inner_t_name) {
                    let mut required = HashSet::new();
                    for (n, f) in inner_t.fields.iter() {
                        if !f.return_type.optional {
                            required.insert(n);
                        }
                    }
                    let mut val_vec: Vec<(PyObject, PyObject)> = Vec::with_capacity(inner.len());
                    for (k, iv) in inner {
                        let key = k.as_str().to_text();
                        if let Some(f) = inner_t.fields.get(&key) {
                            required.remove(&key);
                            val_vec.push((
                                key.into_py(py),
                                input_value_to_python(py, &f.return_type, all_inputs, iv)?,
                            ));
                        }
                    }
                    if !required.is_empty() {
                        bail!("Missing required fields {:?}", required)
                    } else {
                        Ok(val_vec.into_py_dict(py)?.into_py(py))
                    }
                } else {
                    bail!("Could not find type {}", inner_t_name)
                }
            }
            _ => bail!("Input non-object to an object input"),
        },
        AggroTypeInfo::Int => match v {
            GqlValue::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Ok(i.into_py(py))
                } else {
                    bail!("Input non-int to an int input")
                }
            }
            _ => bail!("Input non-int to an int input"),
        },
        AggroTypeInfo::Float => match v {
            GqlValue::Number(n) => {
                if let Some(f) = n.as_f64() {
                    Ok(f.into_py(py))
                } else {
                    bail!("Input non-float to a float input")
                }
            }
            _ => bail!("Input non-float to a float input"),
        },
        AggroTypeInfo::String => match v {
            GqlValue::String(i) => Ok(i.clone().into_py(py)),
            _ => bail!("Input non-string to a string input"),
        },
        AggroTypeInfo::Binary => match v {
            GqlValue::String(i) => Ok(PyBytes::new(
                py,
                &base64::engine::general_purpose::STANDARD.decode(i.as_str())?,
            )
            .into_py(py)),
            _ => bail!("Binary input expected base64 string"),
        },
        AggroTypeInfo::Datetime => match v {
            GqlValue::String(i) => {
                let py_obj: NaiveDateTime = DateTime::parse_from_rfc3339(i.as_str())?.naive_utc();
                Ok(py_obj.into_py(py))
            }
            _ => bail!("Datetime input expected string"),
        },
        AggroTypeInfo::Uuid => match v {
            GqlValue::String(i) => Ok(Uuid::parse_str(i.as_str())?.to_string().into_py(py)),
            _ => bail!("Uuid input expected string"),
        },
        AggroTypeInfo::Boolean => match v {
            GqlValue::Boolean(i) => Ok(i.into_py(py)),
            _ => bail!("Input non-bool to a bool input"),
        },
        AggroTypeInfo::Any => match v {
            GqlValue::Null => Ok(py.None()),
            GqlValue::Boolean(b) => Ok(b.into_py(py)),
            GqlValue::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Ok(i.into_py(py))
                } else if let Some(f) = n.as_f64() {
                    Ok(f.into_py(py))
                } else {
                    Ok(py.None())
                }
            }
            GqlValue::String(s) => Ok(s.clone().into_py(py)),
            GqlValue::List(vals) => {
                let mut vec = Vec::with_capacity(vals.len());
                for val in vals {
                    vec.push(input_value_to_python(py, t, all_inputs, val)?);
                }
                Ok(PyList::new(py, vec)?.into_py(py))
            }
            GqlValue::Object(vals) => {
                let mut map = BTreeMap::new();
                for (k, val) in vals {
                    map.insert(k.as_str(), input_value_to_python(py, t, all_inputs, val)?);
                }
                Ok(map.into_py_dict(py)?.into_py(py))
            }
            _ => bail!("Unsupported value for Any type"),
        },
    }
}

/// Collect arguments from an ObjectAccessor into Python dict entries.
pub(crate) fn collect_arguments_for_python(
    py: Python,
    all_inputs: &Arc<BTreeMap<Text, AggroObject>>,
    field: &AggroField,
    args: &ObjectAccessor<'_>,
) -> PuffResult<BTreeMap<Text, PyObject>> {
    let mut ret = BTreeMap::new();
    for (arg_name, arg_def) in &field.arguments {
        if let Some(val) = args.get(arg_name.as_str()) {
            let gql_val = val.as_value().clone();
            ret.insert(
                arg_name.clone(),
                input_value_to_python(py, &arg_def.param_type, all_inputs, &gql_val)?,
            );
        }
    }
    Ok(ret)
}

// ---------------------------------------------------------------------------
// Key building for correlation (unchanged from original)
// ---------------------------------------------------------------------------

fn key_from_extracted<I: Iterator<Item = GqlValue>>(row_iter: &mut I, len: usize) -> Vec<u8> {
    let v = if len == 1 {
        row_iter.next().unwrap_or(GqlValue::Null)
    } else {
        let mut obj = IndexMap::with_capacity(len);
        for c in 0..len {
            obj.insert(
                async_graphql::Name::new(*NUMBERS.get(c).unwrap()),
                row_iter.next().unwrap_or(GqlValue::Null),
            );
        }
        GqlValue::Object(obj)
    };

    serde_json::to_vec(&v).expect("Could not make correlation Key")
}

// ---------------------------------------------------------------------------
// Layer-based resolution engine
// ---------------------------------------------------------------------------

enum PythonMethodResult {
    SqlQuery(String, Vec<PythonSqlValue>),
    PythonList(Py<PyList>),
}

pub enum LookAheadFields {
    Terminal(BTreeMap<Text, PyObject>),
    Nested(BTreeMap<Text, PyObject>, BTreeMap<Text, LookAheadFields>),
}

fn clone_pyobject_btreemap(py: Python, map: &BTreeMap<Text, PyObject>) -> BTreeMap<Text, PyObject> {
    map.iter()
        .map(|(k, v)| (k.clone(), v.clone_ref(py)))
        .collect()
}

impl Clone for LookAheadFields {
    fn clone(&self) -> Self {
        Python::with_gil(|py| match self {
            LookAheadFields::Terminal(args) => {
                LookAheadFields::Terminal(clone_pyobject_btreemap(py, args))
            }
            LookAheadFields::Nested(args, children) => {
                LookAheadFields::Nested(clone_pyobject_btreemap(py, args), children.clone())
            }
        })
    }
}

impl LookAheadFields {
    pub fn arguments(&self) -> &BTreeMap<Text, PyObject> {
        match self {
            LookAheadFields::Terminal(args) => args,
            LookAheadFields::Nested(args, _) => args,
        }
    }
}

/// Build a LookAheadFields from a ResolverContext (inspects the selection set).
pub fn build_look_ahead_from_ctx(
    py: Python,
    field: &AggroField,
    ctx: &async_graphql::Context<'_>,
    all_inputs: &Arc<BTreeMap<Text, AggroObject>>,
    all_objects: &Arc<BTreeMap<Text, AggroObject>>,
    args: &ObjectAccessor<'_>,
) -> PuffResult<LookAheadFields> {
    let py_args = collect_arguments_for_python(py, all_inputs, field, args)?;

    let has_children = ctx.field().selection_set().count() > 0;

    if has_children {
        let t = match &field.return_type.type_info {
            AggroTypeInfo::Object(t) => t,
            AggroTypeInfo::List(inner) => match &inner.type_info {
                AggroTypeInfo::Object(t) => t,
                _ => return Ok(LookAheadFields::Terminal(py_args)),
            },
            _ => return Ok(LookAheadFields::Terminal(py_args)),
        };

        if let Some(obj) = all_objects.get(t) {
            let mut children = BTreeMap::new();
            for sel_field in ctx.field().selection_set() {
                let child_name = sel_field.name().to_text();
                if let Some(child_field) = obj.fields.get(&child_name) {
                    let child_args =
                        collect_child_args_for_python(py, all_inputs, child_field, &sel_field)?;
                    let child_has_children = sel_field.selection_set().count() > 0;
                    if child_has_children {
                        let child_laf = build_child_look_ahead(
                            py,
                            child_field,
                            &sel_field,
                            all_inputs,
                            all_objects,
                            child_args,
                        )?;
                        children.insert(child_name, child_laf);
                    } else {
                        children.insert(child_name, LookAheadFields::Terminal(child_args));
                    }
                }
            }
            Ok(LookAheadFields::Nested(py_args, children))
        } else {
            Ok(LookAheadFields::Terminal(py_args))
        }
    } else {
        Ok(LookAheadFields::Terminal(py_args))
    }
}

fn collect_child_args_for_python(
    py: Python,
    all_inputs: &Arc<BTreeMap<Text, AggroObject>>,
    field: &AggroField,
    sel_field: &async_graphql::SelectionField<'_>,
) -> PuffResult<BTreeMap<Text, PyObject>> {
    let mut ret = BTreeMap::new();
    if let Ok(args) = sel_field.arguments() {
        for (arg_name, arg_def) in &field.arguments {
            if let Some((_name, val)) = args
                .iter()
                .find(|(name, _)| name.as_str() == arg_name.as_str())
            {
                ret.insert(
                    arg_name.clone(),
                    input_value_to_python(py, &arg_def.param_type, all_inputs, val)?,
                );
            }
        }
    }
    Ok(ret)
}

fn build_child_look_ahead(
    py: Python,
    field: &AggroField,
    sel_field: &async_graphql::SelectionField<'_>,
    all_inputs: &Arc<BTreeMap<Text, AggroObject>>,
    all_objects: &Arc<BTreeMap<Text, AggroObject>>,
    py_args: BTreeMap<Text, PyObject>,
) -> PuffResult<LookAheadFields> {
    let t = match &field.return_type.type_info {
        AggroTypeInfo::Object(t) => t,
        AggroTypeInfo::List(inner) => match &inner.type_info {
            AggroTypeInfo::Object(t) => t,
            _ => return Ok(LookAheadFields::Terminal(py_args)),
        },
        _ => return Ok(LookAheadFields::Terminal(py_args)),
    };

    if let Some(obj) = all_objects.get(t) {
        let mut children = BTreeMap::new();
        for child_sel in sel_field.selection_set() {
            let child_name = child_sel.name().to_text();
            if let Some(child_field) = obj.fields.get(&child_name) {
                let child_args =
                    collect_child_args_for_python(py, all_inputs, child_field, &child_sel)?;
                let child_has_children = child_sel.selection_set().count() > 0;
                if child_has_children {
                    let child_laf = build_child_look_ahead(
                        py,
                        child_field,
                        &child_sel,
                        all_inputs,
                        all_objects,
                        child_args,
                    )?;
                    children.insert(child_name, child_laf);
                } else {
                    children.insert(child_name, LookAheadFields::Terminal(child_args));
                }
            }
        }
        Ok(LookAheadFields::Nested(py_args, children))
    } else {
        Ok(LookAheadFields::Terminal(py_args))
    }
}

// The core layer-resolution function (ported from original)

pub fn returned_values_into_stream<'a>(
    rows: Arc<dyn ExtractValues + Send + Sync>,
    look_ahead: &'a LookAheadFields,
    aggro_field: &'a AggroField,
    all_objects: Arc<BTreeMap<Text, AggroObject>>,
    aggro_context: &'a AggroContext,
    layer_cache: Py<PyDict>,
) -> futures_util::future::BoxFuture<'a, PuffResult<Vec<AggroValue>>> {
    do_returned_values_into_stream(
        rows,
        look_ahead,
        aggro_field,
        all_objects,
        aggro_context,
        layer_cache,
    )
    .boxed()
}

pub async fn do_returned_values_into_stream(
    rows: Arc<dyn ExtractValues + Send + Sync>,
    look_ahead: &'_ LookAheadFields,
    aggro_field: &'_ AggroField,
    all_objects: Arc<BTreeMap<Text, AggroObject>>,
    aggro_context: &AggroContext,
    layer_cache: Py<PyDict>,
) -> PuffResult<Vec<AggroValue>> {
    let type_info = aggro_field.return_type.type_info.clone();
    let class_method = Python::with_gil(|py| {
        aggro_field
            .producer_method
            .as_ref()
            .map(|o| o.clone_ref(py))
    });
    let aggro_value_optional = aggro_field.return_type.optional;
    let aggro_value_is_list = aggro_field.return_type.type_info.is_list();
    if rows.len() == 0 {
        return Ok(vec![]);
    }
    let (args, child_fields) = match look_ahead {
        LookAheadFields::Nested(args, children) => {
            let ret_type = match type_info {
                AggroTypeInfo::List(l) => match l.type_info {
                    AggroTypeInfo::Object(l) => all_objects.get(&l),
                    ref at => {
                        bail!(
                            "Attempted to look up children {:?} on a list of scalar values {} {:?}",
                            children.keys().collect::<Vec<_>>(),
                            aggro_field.name,
                            at
                        )
                    }
                },
                AggroTypeInfo::Object(ref l) => all_objects.get(l),
                ref at => {
                    bail!(
                        "Attempted to look up children {:?} on a scalar value {} {:?}",
                        children.keys().collect::<Vec<_>>(),
                        aggro_field.name,
                        at
                    )
                }
            };
            let mut child_fields = Vec::with_capacity(children.len());
            if let Some(obj) = ret_type {
                for (child, nested_lookahead) in children.iter() {
                    if let Some(field) = obj.fields.get(child) {
                        child_fields.push((field, nested_lookahead))
                    } else {
                        bail!(
                            "Could not find child field {} type for {}",
                            child,
                            aggro_field.name
                        )
                    }
                }
            } else {
                bail!("Could not find return type for {}", aggro_field.name)
            }
            (args, child_fields)
        }
        LookAheadFields::Terminal(args) => (args, Vec::new()),
    };

    let mut child_depends_on_vec = HashSet::new();
    for (f, _) in child_fields.iter().copied() {
        for x in &f.depends_on {
            child_depends_on_vec.insert(x.clone());
        }
    }
    let children_require = child_depends_on_vec.into_iter().collect::<Vec<_>>();

    // -----------------------------------------------------------------------
    // FAST PATH: If this field has a static SQL template (@sql decorator),
    // execute directly in Rust without calling Python at all.
    // -----------------------------------------------------------------------
    if let Some(ref sql) = aggro_field.sql_template {
        let conn_mutex = aggro_context
            .connection()
            .ok_or_else(|| anyhow!("SQL fast path: Postgres is not configured"))?;
        let conn = conn_mutex.lock().await;

        // Build params from GraphQL arguments (pure Rust, no Python)
        let mut params: Vec<RustSqlValue> = aggro_field
            .sql_arg_names
            .iter()
            .map(|name| {
                let val = Python::with_gil(|py| {
                    args.get(&name.to_text())
                        .map(|pyobj| {
                            // Convert PyObject argument back to GqlValue for the Rust path
                            crate::graphql::row_return::convert_pyany_to_value(pyobj.bind(py))
                                .unwrap_or(GqlValue::Null)
                        })
                        .unwrap_or(GqlValue::Null)
                });
                gql_value_to_rust_sql(&val)
            })
            .collect();

        // For correlated child fields, collect parent key values and append
        // them as an array parameter
        let method_corr =
            if !aggro_field.sql_parent_keys.is_empty() && !aggro_field.sql_child_keys.is_empty() {
                let parent_vals = rows.extract_values(&aggro_field.sql_parent_keys)?;
                // Collect all parent key values into arrays for ANY($N)
                let num_keys = aggro_field.sql_parent_keys.len();
                for key_idx in 0..num_keys {
                    let key_values: Vec<GqlValue> = parent_vals
                        .iter()
                        .filter_map(|row| row.as_ref().and_then(|cols| cols.get(key_idx).cloned()))
                        .collect();
                    // Build a GqlValue::List and convert
                    let list_val = GqlValue::List(key_values);
                    params.push(gql_value_to_rust_sql(&list_val));
                }
                Some((
                    aggro_field.sql_parent_keys.clone(),
                    aggro_field.sql_child_keys.clone(),
                ))
            } else {
                None
            };

        let (statement, result_rows) = execute_rust_native(&conn, sql.clone(), params).await?;
        let rr: Arc<dyn ExtractValues + Send + Sync> =
            Arc::new(PostgresResultRows::new(statement, result_rows));

        // Process results identically to the normal path
        match method_corr {
            None => {
                let parent_row_len = rows.len();
                let children_vec = if child_fields.is_empty() {
                    if let Some(col) = aggro_field.value_from_column.as_ref() {
                        let vals = rr.extract_values(std::slice::from_ref(col))?;
                        vals.into_iter()
                            .map(|val| {
                                val.and_then(|v| v.into_iter().next())
                                    .unwrap_or(GqlValue::Null)
                            })
                            .collect()
                    } else {
                        rr.extract_first()?
                    }
                } else {
                    build_child_objects(rr, &child_fields, &all_objects, aggro_context).await?
                };
                let mut final_vec = Vec::with_capacity(parent_row_len);
                if aggro_value_is_list {
                    final_vec.push(GqlValue::List(children_vec));
                    for _ in 1..parent_row_len {
                        final_vec.push(GqlValue::List(vec![]));
                    }
                } else {
                    let mut child_iter = children_vec.into_iter();
                    for _ in 0..parent_row_len {
                        if let Some(c) = child_iter.next() {
                            final_vec.push(c)
                        } else if aggro_value_optional {
                            final_vec.push(GqlValue::Null)
                        } else {
                            bail!(
                                "Missing value in return for field {} which is not optional.",
                                aggro_field.name
                            );
                        }
                    }
                }
                return Ok(final_vec);
            }
            Some((parent_cor, cor)) => {
                let parent_vals = rows.extract_values(&parent_cor)?;
                if parent_vals.iter().all(|f| f.is_none()) {
                    return Ok(parent_vals.iter().map(|_| GqlValue::Null).collect());
                }
                let mapped_children = if child_fields.is_empty() {
                    let mut rows_to_get = cor.clone();
                    if let Some(col) = aggro_field.value_from_column.as_ref() {
                        rows_to_get.push(col.clone())
                    }
                    let vals: Vec<Vec<GqlValue>> = rr
                        .extract_values(rows_to_get.as_slice())?
                        .into_iter()
                        .flatten()
                        .collect();
                    let mut return_vec = BTreeMap::new();
                    for v in vals {
                        let mut row_iter = v.into_iter();
                        let cor_val = key_from_extracted(&mut row_iter, cor.len());
                        let value = if aggro_field.return_type.optional {
                            row_iter.next().unwrap_or(GqlValue::Null)
                        } else {
                            row_iter.next().ok_or(anyhow!(
                                "Could not find a value for non-optional field {}",
                                aggro_field.name
                            ))?
                        };
                        return_vec.insert(cor_val, value);
                    }
                    return_vec
                } else {
                    build_correlated_child_objects(
                        rr,
                        &child_fields,
                        &all_objects,
                        aggro_context,
                        &cor,
                        aggro_value_is_list,
                    )
                    .await?
                };
                let mut final_vec = Vec::with_capacity(parent_vals.len());
                for parent in parent_vals {
                    if let Some(p) = parent {
                        let row_cor_len = p.len();
                        let mut row_cor_iter = p.into_iter();
                        let key = key_from_extracted(&mut row_cor_iter, row_cor_len);
                        let r = mapped_children.get(&key).cloned();
                        let val = if aggro_value_is_list {
                            r.unwrap_or_else(|| GqlValue::List(vec![]))
                        } else if aggro_value_optional {
                            r.unwrap_or(GqlValue::Null)
                        } else {
                            r.ok_or(anyhow!(
                                "Missing value in return for field {} which is not optional.",
                                aggro_field.name
                            ))?
                        };
                        final_vec.push(val)
                    } else {
                        final_vec.push(GqlValue::Null)
                    }
                }
                return Ok(final_vec);
            }
        }
    }

    match class_method {
        Some(cm) => {
            let result = {
                if !aggro_field.is_async && aggro_field.safe_without_context {
                    let py_extractor = PyContext::new(
                        rows.clone(),
                        aggro_context.auth(),
                        None,
                        layer_cache,
                        children_require,
                    );
                    Python::with_gil(|py| {
                        let arg_dict = args_to_py_dict(py, args)?;
                        cm.call_bound(py, (py_extractor.clone(),), Some(&arg_dict))
                    })?
                } else {
                    let py_dispatcher = with_puff_context(|ctx| ctx.python_dispatcher());

                    let rec = if let Some(s) = aggro_context.maybe_connection() {
                        let conn = s.lock().await;
                        let py_extractor = PyContext::new(
                            rows.clone(),
                            aggro_context.auth(),
                            Some(conn.clone()),
                            layer_cache,
                            children_require,
                        );
                        Python::with_gil(|py| {
                            let arg_dict = args_to_py_dict(py, args)?;
                            if aggro_field.is_async {
                                py_dispatcher.dispatch_asyncio(
                                    cm,
                                    (py_extractor,),
                                    Some(arg_dict.unbind()),
                                )
                            } else {
                                py_dispatcher.dispatch(cm, (py_extractor,), Some(arg_dict.unbind()))
                            }
                        })?
                    } else {
                        let py_extractor = PyContext::new(
                            rows.clone(),
                            aggro_context.auth(),
                            None,
                            layer_cache,
                            children_require,
                        );
                        Python::with_gil(|py| {
                            let arg_dict = args_to_py_dict(py, args)?;
                            if aggro_field.is_async {
                                py_dispatcher.dispatch_asyncio(
                                    cm,
                                    (py_extractor,),
                                    Some(arg_dict.unbind()),
                                )
                            } else {
                                py_dispatcher.dispatch(cm, (py_extractor,), Some(arg_dict.unbind()))
                            }
                        })?
                    };
                    rec.await??
                }
            };

            if child_fields.is_empty()
                && aggro_field.value_from_column.is_none()
                && !aggro_value_is_list
                && supports_direct_value_fast_path(&aggro_field.return_type.type_info)
            {
                let broadcast = Python::with_gil(|py| -> PuffResult<Option<Vec<GqlValue>>> {
                    let py_res = result.bind(py);
                    if py_res.downcast::<PyTuple>().is_ok() {
                        return Ok(None);
                    }

                    let direct_value = crate::graphql::row_return::convert_pyany_to_value(py_res)?;
                    Ok(Some(vec![direct_value; rows.len()]))
                })?;

                if let Some(values) = broadcast {
                    return Ok(values);
                }
            }

            let (method_result, method_corr) = Python::with_gil(|py| {
                let py_res = result.bind(py);
                if let Ok((_elp, q, l)) =
                    py_res.extract::<(Bound<'_, PyAny>, Bound<'_, PyString>, Bound<'_, PyList>)>()
                {
                    let v = l
                        .iter()
                        .map(|f| PythonSqlValue::new(f.to_object(py)))
                        .collect::<Vec<_>>();
                    PuffResult::Ok((PythonMethodResult::SqlQuery(q.to_string(), v), None))
                } else if let Ok((_elp, q, l, parent_cor, child_cor)) = py_res.extract::<(
                    Bound<'_, PyAny>,
                    Bound<'_, PyString>,
                    Bound<'_, PyList>,
                    Vec<Text>,
                    Vec<Text>,
                )>() {
                    let v = l
                        .iter()
                        .map(|f| PythonSqlValue::new(f.to_object(py)))
                        .collect::<Vec<_>>();
                    Ok((
                        PythonMethodResult::SqlQuery(q.to_string(), v),
                        Some((parent_cor, child_cor)),
                    ))
                } else if let Ok((_elp, py_list)) =
                    py_res.extract::<(Bound<'_, PyAny>, Bound<'_, PyList>)>()
                {
                    Ok((PythonMethodResult::PythonList(py_list.unbind()), None))
                } else if let Ok((_elp, py_list, parent_cor, child_cor)) =
                    py_res.extract::<(Bound<'_, PyAny>, Bound<'_, PyList>, Vec<Text>, Vec<Text>)>()
                {
                    Ok((
                        PythonMethodResult::PythonList(py_list.unbind()),
                        Some((parent_cor, child_cor)),
                    ))
                } else if aggro_value_is_list {
                    if let Ok(l) = py_res.downcast::<PyList>() {
                        Ok((PythonMethodResult::PythonList(l.clone().unbind()), None))
                    } else {
                        bail!("Expected to return a list.")
                    }
                } else {
                    let parent_len = rows.len();
                    let mut new_v = Vec::with_capacity(parent_len);
                    for _x in 0..parent_len {
                        new_v.push(py_res.to_object(py));
                    }
                    Ok((
                        PythonMethodResult::PythonList(PyList::new(py, new_v)?.unbind()),
                        None,
                    ))
                }
            })?;

            let rr: Arc<dyn ExtractValues + Send + Sync> = match method_result {
                PythonMethodResult::PythonList(l) => Arc::new(PythonResultRows { py_list: l }),
                PythonMethodResult::SqlQuery(q, params) => {
                    let conn_mutex = aggro_context.connection().ok_or_else(|| {
                        anyhow!("SQL query returned but Postgres is not configured")
                    })?;
                    let conn = conn_mutex.lock().await;
                    let (statement, rows) = execute_rust(&conn, q, params).await?;
                    Arc::new(PostgresResultRows::new(statement, rows))
                }
            };

            match method_corr {
                None => {
                    let parent_row_len = rows.len();
                    let children_vec = if child_fields.is_empty() {
                        if let Some(col) = aggro_field.value_from_column.as_ref() {
                            let vals = rr.extract_values(std::slice::from_ref(col))?;
                            vals.into_iter()
                                .map(|val| {
                                    val.and_then(|v| v.into_iter().next())
                                        .unwrap_or(GqlValue::Null)
                                })
                                .collect()
                        } else {
                            rr.extract_first()?
                        }
                    } else {
                        build_child_objects(rr, &child_fields, &all_objects, aggro_context).await?
                    };
                    let mut final_vec = Vec::with_capacity(parent_row_len);
                    if aggro_value_is_list {
                        final_vec.push(GqlValue::List(children_vec));
                        for _ in 1..parent_row_len {
                            final_vec.push(GqlValue::List(vec![]));
                        }
                    } else {
                        let mut child_iter = children_vec.into_iter();
                        for _ in 0..parent_row_len {
                            if let Some(c) = child_iter.next() {
                                final_vec.push(c)
                            } else if aggro_value_optional {
                                final_vec.push(GqlValue::Null)
                            } else {
                                bail!(
                                    "Missing value in return for field {} which is not optional.",
                                    aggro_field.name
                                );
                            }
                        }
                    }
                    Ok(final_vec)
                }
                Some((parent_cor, cor)) => {
                    let parent_vals = rows.extract_values(&parent_cor)?;
                    if parent_vals.iter().all(|f| f.is_none()) {
                        Ok(parent_vals.iter().map(|_| GqlValue::Null).collect())
                    } else {
                        let mapped_children = if child_fields.is_empty() {
                            let mut rows_to_get = cor.clone();
                            if let Some(col) = aggro_field.value_from_column.as_ref() {
                                rows_to_get.push(col.clone())
                            }
                            let vals: Vec<Vec<GqlValue>> = rr
                                .extract_values(rows_to_get.as_slice())?
                                .into_iter()
                                .flatten()
                                .collect();
                            let mut return_vec = BTreeMap::new();
                            for v in vals {
                                let mut row_iter = v.into_iter();
                                let cor_val = key_from_extracted(&mut row_iter, cor.len());
                                let value = if aggro_field.return_type.optional {
                                    row_iter.next().unwrap_or(GqlValue::Null)
                                } else {
                                    row_iter.next().ok_or(anyhow!(
                                        "Could not find a value for non-optional field {}",
                                        aggro_field.name
                                    ))?
                                };
                                return_vec.insert(cor_val, value);
                            }
                            return_vec
                        } else {
                            build_correlated_child_objects(
                                rr,
                                &child_fields,
                                &all_objects,
                                aggro_context,
                                &cor,
                                aggro_value_is_list,
                            )
                            .await?
                        };
                        let mut final_vec = Vec::with_capacity(parent_vals.len());
                        for parent in parent_vals {
                            if let Some(p) = parent {
                                let row_cor_len = p.len();
                                let mut row_cor_iter = p.into_iter();
                                let key = key_from_extracted(&mut row_cor_iter, row_cor_len);
                                let r = mapped_children.get(&key).cloned();
                                let val = if aggro_value_is_list {
                                    r.unwrap_or_else(|| GqlValue::List(vec![]))
                                } else if aggro_value_optional {
                                    r.unwrap_or(GqlValue::Null)
                                } else {
                                    r.ok_or(anyhow!(
                                        "Missing value in return for field {} which is not optional.",
                                        aggro_field.name
                                    ))?
                                };
                                final_vec.push(val)
                            } else {
                                final_vec.push(GqlValue::Null)
                            }
                        }
                        Ok(final_vec)
                    }
                }
            }
        }
        None => {
            if child_fields.is_empty() {
                let vals = if let Some(col) = aggro_field.value_from_column.as_ref() {
                    let vals = rows.extract_values(std::slice::from_ref(col))?;
                    let mut ret_val = Vec::with_capacity(vals.len());
                    for (pos, val) in vals.into_iter().enumerate() {
                        if let Some(v) = val {
                            if let Some(s) = v.into_iter().next() {
                                ret_val.push(s);
                            } else if aggro_value_optional {
                                ret_val.push(GqlValue::Null);
                            } else {
                                bail!(
                                    "Missing value for field {} row {} column {:?}",
                                    aggro_field.name,
                                    pos,
                                    &aggro_field.value_from_column
                                );
                            }
                        } else {
                            ret_val.push(GqlValue::Null)
                        }
                    }
                    ret_val
                } else {
                    rows.extract_first()?
                };
                Ok(vals)
            } else {
                build_child_objects_from_rows(rows, &child_fields, &all_objects, aggro_context)
                    .await
            }
        }
    }
}

async fn build_child_objects(
    rr: Arc<dyn ExtractValues + Send + Sync>,
    child_fields: &[(&AggroField, &LookAheadFields)],
    all_objects: &Arc<BTreeMap<Text, AggroObject>>,
    aggro_context: &AggroContext,
) -> PuffResult<Vec<GqlValue>> {
    let mut objs: Vec<Option<IndexMap<async_graphql::Name, GqlValue>>> =
        Vec::with_capacity(rr.len());
    for present in rr.row_presence() {
        if !present {
            objs.push(None)
        } else {
            objs.push(Some(IndexMap::with_capacity(child_fields.len())))
        }
    }
    if objs.iter().all(|f| f.is_none()) {
        return Ok(objs.iter().map(|_| GqlValue::Null).collect());
    }
    let mut to_compute = Vec::with_capacity(child_fields.len());
    let new_layer_cache: Py<PyDict> = Python::with_gil(|py| PyDict::new(py).unbind());
    for (child, new_lookahead) in child_fields.iter().copied() {
        let child_rows = rr.clone();
        let child_objects = all_objects.clone();
        let this_layer_cache = Python::with_gil(|py| new_layer_cache.clone_ref(py));
        let fut = async move {
            let children = returned_values_into_stream(
                child_rows,
                new_lookahead,
                child,
                child_objects,
                aggro_context,
                this_layer_cache,
            )
            .await;
            (child, children)
        };
        to_compute.push(fut)
    }
    let children_results = join_all(to_compute).await;
    for (child, children) in children_results {
        for (obj, new_value) in objs.iter_mut().zip(children?) {
            if new_value == GqlValue::Null && !child.return_type.optional {
                bail!(
                    "Missing value in return for field {} which is not optional.",
                    child.name
                );
            } else if let Some(o) = obj {
                o.insert(async_graphql::Name::new(child.name.as_str()), new_value);
            }
        }
    }
    Ok(objs
        .into_iter()
        .map(|f| match f {
            Some(o) => GqlValue::Object(o),
            None => GqlValue::Null,
        })
        .collect())
}

async fn build_correlated_child_objects(
    rr: Arc<dyn ExtractValues + Send + Sync>,
    child_fields: &[(&AggroField, &LookAheadFields)],
    all_objects: &Arc<BTreeMap<Text, AggroObject>>,
    aggro_context: &AggroContext,
    cor: &[Text],
    is_list: bool,
) -> PuffResult<BTreeMap<Vec<u8>, GqlValue>> {
    let mut objs: Vec<Option<IndexMap<async_graphql::Name, GqlValue>>> =
        Vec::with_capacity(rr.len());
    for present in rr.row_presence() {
        if !present {
            objs.push(None);
        } else {
            objs.push(Some(IndexMap::with_capacity(child_fields.len())));
        }
    }
    let mut to_compute = Vec::with_capacity(child_fields.len());
    let new_layer_cache: Py<PyDict> = Python::with_gil(|py| PyDict::new(py).unbind());
    for (child, new_lookahead) in child_fields.iter().copied() {
        let child_rows = rr.clone();
        let child_objects = all_objects.clone();
        let this_layer_cache = Python::with_gil(|py| new_layer_cache.clone_ref(py));
        let fut = async move {
            let children = returned_values_into_stream(
                child_rows,
                new_lookahead,
                child,
                child_objects,
                aggro_context,
                this_layer_cache,
            )
            .await;
            (child, children)
        };
        to_compute.push(fut)
    }
    let children_results = join_all(to_compute).await;
    for (child, children) in children_results {
        for (obj, new_value) in objs.iter_mut().zip(children?) {
            if new_value == GqlValue::Null && !child.return_type.optional {
                bail!(
                    "Missing value in return for field {} which is not optional.",
                    child.name
                );
            } else if let Some(o) = obj {
                o.insert(async_graphql::Name::new(child.name.as_str()), new_value);
            }
        }
    }

    let cor_vals: Vec<Vec<GqlValue>> = rr.extract_values(cor)?.into_iter().flatten().collect();
    let cor_len = cor.len();
    if is_list {
        let mut hm = BTreeMap::new();
        for (obj, row_cor) in objs.into_iter().zip(cor_vals) {
            let mut row_cor_iter = row_cor.into_iter();
            let key = key_from_extracted(&mut row_cor_iter, cor_len);
            hm.entry(key).or_insert_with(Vec::new).push(match obj {
                Some(o) => GqlValue::Object(o),
                None => GqlValue::Null,
            });
        }
        Ok(hm
            .into_iter()
            .map(|(k, v)| (k, GqlValue::List(v)))
            .collect())
    } else {
        Ok(objs
            .into_iter()
            .zip(cor_vals)
            .map(|(val, row_cor)| {
                let mut row_cor_iter = row_cor.into_iter();
                let cor_val = key_from_extracted(&mut row_cor_iter, cor_len);
                let ag_val = match val {
                    Some(o) => GqlValue::Object(o),
                    None => GqlValue::Null,
                };
                (cor_val, ag_val)
            })
            .collect())
    }
}

async fn build_child_objects_from_rows(
    rows: Arc<dyn ExtractValues + Send + Sync>,
    child_fields: &[(&AggroField, &LookAheadFields)],
    all_objects: &Arc<BTreeMap<Text, AggroObject>>,
    aggro_context: &AggroContext,
) -> PuffResult<Vec<GqlValue>> {
    let mut objs: Vec<IndexMap<async_graphql::Name, GqlValue>> = Vec::with_capacity(rows.len());
    for _x in 0..rows.len() {
        objs.push(IndexMap::with_capacity(child_fields.len()));
    }

    let mut to_compute = Vec::with_capacity(child_fields.len());
    let new_layer_cache: Py<PyDict> = Python::with_gil(|py| PyDict::new(py).unbind());
    for (child, new_lookahead) in child_fields.iter().copied() {
        let child_rows = rows.clone();
        let child_objects = all_objects.clone();
        let this_layer_cache = Python::with_gil(|py| new_layer_cache.clone_ref(py));
        let fut = async move {
            let children = returned_values_into_stream(
                child_rows,
                new_lookahead,
                child,
                child_objects,
                aggro_context,
                this_layer_cache,
            )
            .await;
            (child, children)
        };
        to_compute.push(fut)
    }
    let children_results = join_all(to_compute).await;
    for (child, children) in children_results {
        for (obj, new_value) in objs.iter_mut().zip(children?) {
            if new_value == GqlValue::Null && !child.return_type.optional {
                bail!(
                    "Missing value in return for field {} which is not optional.",
                    child.name
                );
            } else {
                obj.insert(async_graphql::Name::new(child.name.as_str()), new_value);
            }
        }
    }

    Ok(objs.into_iter().map(GqlValue::Object).collect())
}

// ---------------------------------------------------------------------------
// SubscriptionSender
// ---------------------------------------------------------------------------

#[pyclass]
pub struct SubscriptionSender {
    gql_config: PuffGraphqlConfig,
    sender: UnboundedSender<GqlValue>,
    look_ahead: Arc<LookAheadFields>,
    field: AggroField,
    all_objs: Arc<BTreeMap<Text, AggroObject>>,
    auth: Option<PyObject>,
    rows: Arc<dyn ExtractValues + Send + Sync>,
}

impl SubscriptionSender {
    pub fn new(
        sender: UnboundedSender<GqlValue>,
        look_ahead: LookAheadFields,
        field: AggroField,
        all_objs: Arc<BTreeMap<Text, AggroObject>>,
        auth: Option<PyObject>,
        rows: Arc<dyn ExtractValues + Send + Sync>,
        gql_config: PuffGraphqlConfig,
    ) -> Self {
        Self {
            sender,
            look_ahead: Arc::new(look_ahead),
            field,
            all_objs,
            auth,
            rows,
            gql_config,
        }
    }
}

#[pymethods]
impl SubscriptionSender {
    fn __call__(&self, py: Python, ret_func: PyObject, new_function: PyObject) {
        let this_lookahead = Arc::clone(&self.look_ahead);
        let this_field = self.field.clone();
        let all_objects = self.all_objs.clone();
        let auth = self.auth.as_ref().map(|o| o.clone_ref(py));
        let this_sender = self.sender.clone();
        let rows = self.rows.clone();
        let context = self.gql_config.new_context(auth);
        let layer_cache = PyDict::new(py).unbind();
        run_python_async(ret_func, async move {
            let mut my_field = this_field;
            my_field.producer_method = Some(new_function);
            my_field.is_async = false;
            my_field.safe_without_context = true;

            let res = returned_values_into_stream(
                rows,
                this_lookahead.as_ref(),
                &my_field,
                all_objects,
                &context,
                layer_cache,
            )
            .await?;
            for r in res {
                if this_sender.send(r).is_err() {
                    return Ok(false);
                }
            }
            Ok(true)
        })
    }
}
