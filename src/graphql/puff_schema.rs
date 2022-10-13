use crate::context::with_puff_context;
use crate::errors::{to_py_error, PuffResult};
use crate::graphql::puff_schema::LookAheadFields::{Nested, Terminal};
use crate::graphql::row_return::{ExtractValues, PostgresResultRows, PythonResultRows};
use crate::graphql::scalar::{AggroScalarValue, AggroValue};
use crate::python::greenlet::greenlet_async;
use crate::python::postgres::PythonSqlValue;
use crate::types::text::ToText;
use crate::types::Text;
use anyhow::{anyhow, bail};

use futures::TryStreamExt;
use futures_util::FutureExt;
use juniper::{
    BoxFuture, ExecutionError, LookAheadArgument, LookAheadMethods, LookAheadSelection,
    LookAheadValue, Object, Value,
};

use pyo3::prelude::*;
use pyo3::types::{IntoPyDict, PyDict, PyList, PyString};
use std::collections::{BTreeMap, HashSet};

use std::sync::Arc;

use crate::graphql;
use tokio::sync::mpsc::UnboundedSender;
use tokio_postgres::Transaction;

static NUMBERS: &'static [&'static str] = &["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];

#[derive(Clone, Debug)]
pub struct AggroContext(Option<Text>);

impl juniper::Context for AggroContext {}

impl AggroContext {
    pub fn new(bearer: Option<Text>) -> Self {
        Self(bearer)
    }

    pub fn token(&self) -> Option<Text> {
        self.0.clone()
    }
}

#[derive(Debug, Clone)]
pub enum AggroTypeInfo {
    String,
    Int,
    Boolean,
    Float,
    Any,
    List(Box<DecodedType>),
    Object(Text),
}

impl AggroTypeInfo {
    fn is_list(&self) -> bool {
        matches!(self, AggroTypeInfo::List(_))
    }
    // fn is_scalar(&self) -> bool {
    //     matches!(
    //         self,
    //         AggroTypeInfo::String
    //             | AggroTypeInfo::Int
    //             | AggroTypeInfo::Boolean
    //             | AggroTypeInfo::Float
    //     )
    // }
    // fn is_object(&self) -> bool {
    //     matches!(self, AggroTypeInfo::Object(_))
    // }
    // fn is_complex(&self) -> bool {
    //     !(self.is_object() || self.is_list())
    // }
}

#[derive(Debug, Clone)]
pub struct DecodedType {
    pub optional: bool,
    pub type_info: AggroTypeInfo,
}

#[derive(Debug, Clone)]
pub struct AggroArgument {
    pub default: Py<PyAny>,
    pub param_type: DecodedType,
}

#[derive(Debug, Clone)]
pub struct AggroField {
    pub name: Text,
    pub return_type: DecodedType,
    pub depends_on: Vec<Text>,
    pub producer_method: Option<Py<PyAny>>,
    pub acceptor_method: Option<Py<PyAny>>,
    pub arguments: BTreeMap<Text, AggroArgument>,
    pub safe_without_context: bool,
    pub default: Py<PyAny>,
}

impl AggroField {
    pub fn as_argument(&self) -> AggroArgument {
        AggroArgument {
            default: self.default.clone(),
            param_type: self.return_type.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AggroObject {
    pub name: Text,
    pub fields: Arc<BTreeMap<Text, AggroField>>,
}

fn decode_type(t: &PyAny) -> PyResult<DecodedType> {
    let optional: bool = t.getattr("optional")?.extract()?;
    let type_info_str: Text = t.getattr("type_info")?.extract()?;
    let type_info = match type_info_str.as_str() {
        "List" => {
            let inner_type_python: &PyAny = t.getattr("inner_type")?.extract()?;
            AggroTypeInfo::List(Box::new(decode_type(inner_type_python)?))
        }
        "String" => AggroTypeInfo::String,
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

pub fn convert(
    schema: &PyAny,
    type_to_description: &PyAny,
) -> PyResult<(BTreeMap<Text, AggroObject>, BTreeMap<Text, AggroObject>)> {
    let (all_types, input_types): (&PyDict, &PyDict) =
        type_to_description.call1((schema,))?.extract()?;

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
    Ok((return_objs, return_inputs))
}

pub fn convert_obj(name: &str, desc: BTreeMap<String, &PyAny>) -> PyResult<AggroObject> {
    let mut object_fields: BTreeMap<Text, AggroField> = BTreeMap::new();

    for (k, field_description) in desc.iter() {
        let args: &PyDict = field_description.getattr("arguments")?.extract()?;
        let mut final_arguments = BTreeMap::new();

        for (param_name, param) in args.iter() {
            let arg = AggroArgument {
                param_type: decode_type(param.getattr("param_type")?)?,
                default: param.getattr("default")?.extract()?,
            };
            final_arguments.insert(param_name.str()?.to_text(), arg);
        }

        let depends_on = field_description
            .getattr("depends_on")?
            .extract::<Option<_>>()?
            .unwrap_or_default();

        let field = AggroField {
            depends_on,
            name: k.into(),
            return_type: decode_type(field_description.getattr("return_type")?)?,
            producer_method: field_description.getattr("producer")?.extract()?,
            acceptor_method: field_description.getattr("acceptor")?.extract()?,
            safe_without_context: field_description
                .getattr("safe_without_context")?
                .extract()?,
            default: field_description.getattr("default")?.extract()?,
            arguments: final_arguments,
        };

        object_fields.insert(k.into(), field);
    }
    return Ok(AggroObject {
        name: name.to_text(),
        fields: Arc::new(object_fields),
    });
}

#[derive(Clone)]
#[pyclass]
pub struct PyContext {
    extractor: Arc<dyn ExtractValues + Send + Sync>,
    bearer: Option<Text>,
}

impl PyContext {
    pub fn new(extractor: Arc<dyn ExtractValues + Send + Sync>, bearer: Option<Text>) -> Self {
        Self { extractor, bearer }
    }

    fn replace_extractor(&self, extractor: Arc<dyn ExtractValues + Send + Sync>) -> Self {
        Self {
            extractor,
            bearer: self.bearer.clone(),
        }
    }
}

#[pymethods]
impl PyContext {
    fn parent_values(&self, py: Python, names: Vec<&PyString>) -> PyResult<PyObject> {
        let rows = to_py_error("Gql Extract", self.extractor.extract_py_values(py, &names))?;
        Ok(rows.into_py(py))
    }

    fn auth_token(&self, py: Python) -> Option<Py<PyString>> {
        self.bearer
            .as_ref()
            .map(|f| PyString::new(py, f.as_str()).into_py(py))
    }
}

fn input_to_python(
    py: Python,
    t: &DecodedType,
    all_inputs: &Arc<BTreeMap<Text, AggroObject>>,
    v: &LookAheadValue<AggroScalarValue>,
) -> PuffResult<Py<PyAny>> {
    match v {
        LookAheadValue::Null => {
            if t.optional {
                return Ok(PyList::empty(py).into_py(py));
            } else {
                bail!("Null supplied to non-optional field");
            }
        }
        _ => (),
    };

    match &t.type_info {
        AggroTypeInfo::List(inner_t) => match v {
            LookAheadValue::List(inner) => {
                let mut val_vec: Vec<PyObject> = Vec::with_capacity(inner.len());
                for iv in inner {
                    val_vec.push(input_to_python(py, inner_t, all_inputs, iv)?);
                }
                PuffResult::Ok(PyList::new(py, val_vec).into_py(py))
            }
            _ => bail!("Input non-list to a list input"),
        },
        AggroTypeInfo::Object(inner_t_name) => match v {
            LookAheadValue::Object(inner) => {
                if let Some(inner_t) = all_inputs.get(inner_t_name) {
                    let mut required = HashSet::new();
                    for (n, f) in inner_t.fields.iter() {
                        if !f.return_type.optional {
                            required.insert(n);
                        }
                    }
                    let mut val_vec: Vec<(PyObject, PyObject)> = Vec::with_capacity(inner.len());
                    for (k, iv) in inner {
                        let key = k.to_text();
                        if let Some(f) = inner_t.fields.get(&key) {
                            required.remove(&key);
                            val_vec.push((
                                key.into_py(py),
                                input_to_python(py, &f.return_type, all_inputs, iv)?,
                            ));
                        }
                    }
                    if required.len() > 0 {
                        bail!("Missing required fields {:?}", required)
                    } else {
                        Ok(val_vec.into_py_dict(py).into_py(py))
                    }
                } else {
                    bail!("Could not find type {}", inner_t_name)
                }
            }
            _ => bail!("Input non-object to a object input"),
        },
        AggroTypeInfo::Int => match v {
            LookAheadValue::Scalar(&AggroScalarValue::Int(i)) => Ok(i.into_py(py)),
            _ => bail!("Input non-int to a int input"),
        },
        AggroTypeInfo::Float => match v {
            LookAheadValue::Scalar(&AggroScalarValue::Float(i)) => Ok(i.into_py(py)),
            _ => bail!("Input non-float to a float input"),
        },
        AggroTypeInfo::String => match v {
            LookAheadValue::Scalar(AggroScalarValue::String(i)) => Ok(i.into_py(py)),
            _ => bail!("Input non-string to a string input"),
        },
        AggroTypeInfo::Boolean => match v {
            LookAheadValue::Scalar(AggroScalarValue::Boolean(i)) => Ok(i.into_py(py)),
            _ => bail!("Input non-bool to a bool input"),
        },
        AggroTypeInfo::Any => match v {
            LookAheadValue::Null => Ok(py.None()),
            LookAheadValue::Scalar(s) => graphql::scalar_to_python(py, s),
            LookAheadValue::List(vals) => {
                let mut v = Vec::with_capacity(vals.len());
                for val in vals {
                    v.push(input_to_python(py, t, all_inputs, val)?);
                }
                Ok(PyList::new(py, v).into_py(py))
            }
            LookAheadValue::Object(vals) => {
                let mut v = BTreeMap::new();
                for (k, val) in vals {
                    v.insert(k, input_to_python(py, t, all_inputs, val)?);
                }
                Ok(v.into_py_dict(py).into_py(py))
            }
            _ => bail!("Input non-bool to a bool input"),
        },
    }
}

fn key_from_extracted<I: Iterator<Item = AggroValue>>(row_iter: &mut I, len: usize) -> Vec<u8> {
    let v = if len == 1 {
        row_iter.next().unwrap_or(AggroValue::Null)
    } else {
        let mut obj = Object::with_capacity(len);
        for c in 0..len {
            obj.add_field(
                *NUMBERS.get(c).unwrap(),
                row_iter.next().unwrap_or(AggroValue::Null),
            );
        }
        AggroValue::Object(obj)
    };

    bincode::serialize(&v).expect("Could not make correlation Key")
}

enum PythonMethodResult {
    SqlQuery(String, Vec<PythonSqlValue>),
    PythonList(Py<PyList>),
}

pub fn returned_values_into_stream<'a>(
    txn: &'a Transaction<'a>,
    rows: Arc<dyn ExtractValues + Send + Sync>,
    look_ahead: &'a LookAheadFields,
    aggro_field: &'a AggroField,
    all_objects: Arc<BTreeMap<Text, AggroObject>>,
    py_context: PyContext,
) -> BoxFuture<'a, PuffResult<Vec<AggroValue>>> {
    do_returned_values_into_stream(txn, rows, look_ahead, aggro_field, all_objects, py_context)
        .boxed()
}

pub async fn do_returned_values_into_stream(
    txn: &Transaction<'_>,
    rows: Arc<dyn ExtractValues + Send + Sync>,
    look_ahead: &'_ LookAheadFields,
    aggro_field: &AggroField,
    all_objects: Arc<BTreeMap<Text, AggroObject>>,
    py_context: PyContext,
) -> PuffResult<Vec<AggroValue>> {
    let type_info = aggro_field.return_type.type_info.clone();
    let class_method = aggro_field.producer_method.clone();
    let aggro_value_optional = aggro_field.return_type.optional;
    let aggro_value_is_list = aggro_field.return_type.type_info.is_list();
    let (args, child_fields) = match look_ahead {
        LookAheadFields::Nested(args, children) => {
            let ret_type = match type_info {
                AggroTypeInfo::List(l) => {
                    match l.type_info {
                        AggroTypeInfo::Object(l) => all_objects.get(&l),
                        at => {
                            bail!("Attempted to look up children {:?} on a list of scalar values {} {:?}", children.keys().collect::<Vec<_>>(), aggro_field.name, at)
                        }
                    }
                }
                AggroTypeInfo::Object(l) => all_objects.get(&l),
                at => {
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
                for (child, nested_lookahad) in children.into_iter() {
                    if let Some(field) = obj.fields.get(&child) {
                        child_fields.push((field.clone(), nested_lookahad))
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

    // Collect children database fields
    let mut child_depends_on_vec = Vec::new();
    for (f, _) in &child_fields {
        child_depends_on_vec.extend_from_slice(&f.depends_on);
    }

    match class_method {
        Some(cm) => {
            let py_extractor = py_context.replace_extractor(rows.clone());
            let result = if aggro_field.safe_without_context {
                Python::with_gil(|py| {
                    let arg_dict = args.into_py_dict(py);
                    cm.call(py, (py_extractor.clone(),), Some(arg_dict))
                })?
            } else {
                let py_dispatcher = with_puff_context(|ctx| ctx.python_dispatcher());
                let rec = Python::with_gil(|py| {
                    let arg_dict = args.into_py_dict(py);
                    py_dispatcher.dispatch(cm, (py_extractor.clone(),), Some(arg_dict))
                })?;

                rec.await??
            };

            let (method_result, method_corr) = Python::with_gil(|py| {
                let py_res = result.as_ref(py);
                if let Ok((_elp, q, l)) = py_res.extract::<(&PyAny, &PyString, &PyList)>() {
                    let v = l
                        .into_iter()
                        .map(|f| PythonSqlValue::new(f.to_object(py)))
                        .collect::<Vec<_>>();
                    PuffResult::Ok((PythonMethodResult::SqlQuery(q.to_string(), v), None))
                } else if let Ok((_elp, q, l, parent_cor, child_cor)) =
                    py_res.extract::<(&PyAny, &PyString, &PyList, Vec<Text>, Vec<Text>)>()
                {
                    let v = l
                        .into_iter()
                        .map(|f| PythonSqlValue::new(f.to_object(py)))
                        .collect::<Vec<_>>();
                    Ok((
                        PythonMethodResult::SqlQuery(q.to_string(), v),
                        Some((parent_cor, child_cor)),
                    ))
                } else if let Ok((_elp, py_list)) = py_res.extract::<(&PyAny, &PyList)>() {
                    Ok((PythonMethodResult::PythonList(py_list.into_py(py)), None))
                } else if let Ok((_elp, py_list, parent_cor, child_cor)) =
                    py_res.extract::<(&PyAny, &PyList, Vec<Text>, Vec<Text>)>()
                {
                    Ok((
                        PythonMethodResult::PythonList(py_list.into_py(py)),
                        Some((parent_cor, child_cor)),
                    ))
                } else {
                    if aggro_value_is_list {
                        if let Ok(l) = py_res.downcast::<PyList>() {
                            Ok((
                                PythonMethodResult::PythonList(
                                    l.into_py(py),
                                ),
                                None,
                            ))
                        } else {
                            bail!("Expected to return a list.")
                        }
                    } else {
                        Ok((
                            PythonMethodResult::PythonList(
                                PyList::new(py, vec![py_res.into_py(py)]).into_py(py),
                            ),
                            None,
                        ))
                    }

                }
            })?;

            let rr: Arc<dyn ExtractValues + Send + Sync> = match method_result {
                PythonMethodResult::PythonList(l) => Arc::new(PythonResultRows { py_list: l }),
                PythonMethodResult::SqlQuery(q, params) => {
                    let statement = txn.prepare(&q).await?;
                    let rowstream = txn.query_raw(&statement, &params).await?;
                    let rows = rowstream.try_collect().await?;
                    Arc::new(PostgresResultRows { statement, rows })
                }
            };

            match method_corr {
                None => {
                    let parent_row_len = rows.len();
                    let children_vec = if child_fields.is_empty() {
                        if aggro_field.depends_on.len() == 0 {
                            rr.extract_first()?
                        } else {
                            let vals = rr.extract_values(&aggro_field.depends_on)?;
                            let mut ret_val = Vec::with_capacity(vals.len());
                            for val in vals {
                                ret_val.push(val.into_iter().next().unwrap_or(AggroValue::Null));
                            }
                            ret_val
                        }
                    } else {
                        let mut objs = Vec::with_capacity(rr.len());
                        for _x in 0..rr.len() {
                            objs.push(juniper::Object::<AggroScalarValue>::with_capacity(
                                child_fields.len(),
                            ))
                        }
                        for (child, new_lookahead) in child_fields {
                            let children = returned_values_into_stream(
                                txn,
                                rr.clone(),
                                new_lookahead,
                                &child,
                                all_objects.clone(),
                                py_extractor.clone(),
                            )
                            .await?;
                            for (obj, new_value) in objs.iter_mut().zip(children) {
                                obj.add_field(child.name.as_str(), new_value);
                            }
                        }
                        objs.into_iter().map(AggroValue::Object).collect::<Vec<_>>()
                    };
                    let mut final_vec = Vec::with_capacity(parent_row_len);

                    if aggro_value_is_list {
                        final_vec.push(AggroValue::List(children_vec));

                        for _ in 1..parent_row_len {
                            final_vec.push(AggroValue::List(vec![]));
                        }
                    } else {
                        let mut child_iter = children_vec.into_iter();

                        for _ in 0..parent_row_len {
                            final_vec.push(child_iter.next().unwrap_or(AggroValue::Null));
                        }
                    }

                    Ok(final_vec)
                }
                Some((parent_cor, cor)) => {
                    let parent_vals = rows.extract_values(&parent_cor)?;
                    let mapped_children = if child_fields.is_empty() {
                        let mut rows_to_get = cor.clone();
                        rows_to_get.extend_from_slice(aggro_field.depends_on.as_slice());
                        let vals = rr.extract_values(rows_to_get.as_slice())?;
                        let mut return_vec = BTreeMap::new();
                        for v in vals {
                            let mut row_iter = v.into_iter();

                            let cor_val = key_from_extracted(&mut row_iter, cor.len());

                            let value = if aggro_field.return_type.optional {
                                row_iter.next().unwrap_or(AggroValue::Null)
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
                        let mut objs = Vec::with_capacity(rr.len());
                        for _x in 0..rr.len() {
                            objs.push(juniper::Object::<AggroScalarValue>::with_capacity(
                                child_fields.len(),
                            ))
                        }
                        for (child, new_lookahead) in child_fields {
                            let children = returned_values_into_stream(
                                txn,
                                rr.clone(),
                                new_lookahead,
                                &child,
                                all_objects.clone(),
                                py_extractor.clone(),
                            )
                            .await?;
                            for (obj, new_value) in objs.iter_mut().zip(children) {
                                obj.add_field(child.name.as_str(), new_value);
                            }
                        }

                        let cor_vals = rr.extract_values(&cor)?;
                        let cor_len = cor.len();
                        if aggro_value_is_list {
                            let mut hm = BTreeMap::new();
                            for (obj, row_cor) in objs.into_iter().zip(cor_vals) {
                                let mut row_cor_iter = row_cor.into_iter();
                                let key = key_from_extracted(&mut row_cor_iter, cor_len);
                                hm.entry(key)
                                    .or_insert_with(|| Vec::with_capacity(1))
                                    .push(AggroValue::Object(obj))
                            }
                            hm.into_iter()
                                .map(|(k, v)| (k, AggroValue::List(v)))
                                .collect::<BTreeMap<_, _>>()
                        } else {
                            objs.into_iter()
                                .zip(cor_vals)
                                .map(|(val, row_cor)| {
                                    let mut row_cor_iter = row_cor.into_iter();
                                    let cor_val = key_from_extracted(&mut row_cor_iter, cor_len);
                                    (cor_val, AggroValue::Object(val))
                                })
                                .collect::<BTreeMap<_, _>>()
                        }
                    };
                    let mut final_vec = Vec::with_capacity(parent_vals.len());
                    for parent in parent_vals {
                        let row_cor_len = parent.len();
                        let mut row_cor_iter = parent.into_iter();
                        let key = key_from_extracted(&mut row_cor_iter, row_cor_len);
                        let r = mapped_children.get(&key).map(|f| f.clone());

                        let val = if aggro_value_is_list {
                            r.unwrap_or_else(|| AggroValue::List(vec![]))
                        } else {
                            if aggro_value_optional {
                                r.unwrap_or(AggroValue::Null)
                            } else {
                                r.ok_or(anyhow!("Missing field {}", aggro_field.name))?
                            }
                        };
                        final_vec.push(val)
                    }
                    Ok(final_vec)
                }
            }
        }
        None => {
            if child_fields.is_empty() {
                let vals = if aggro_field.depends_on.len() == 0 {
                    rows.extract_first()?
                } else {
                    let vals = rows.extract_values(&aggro_field.depends_on)?;
                    let mut ret_val = Vec::with_capacity(vals.len());
                    for val in vals {
                        ret_val.push(val.into_iter().next().unwrap_or(AggroValue::Null));
                    }
                    ret_val
                };

                Ok(vals)
            } else {
                let mut objs = Vec::with_capacity(rows.len());
                for _x in 0..rows.len() {
                    objs.push(juniper::Object::<AggroScalarValue>::with_capacity(
                        child_fields.len(),
                    ))
                }
                for (child, new_lookahead) in child_fields {
                    let children = returned_values_into_stream(
                        txn,
                        rows.clone(),
                        new_lookahead,
                        &child,
                        all_objects.clone(),
                        py_context.clone(),
                    )
                    .await?;
                    for (obj, new_value) in objs.iter_mut().zip(children) {
                        obj.add_field(child.name.as_str(), new_value);
                    }
                }

                Ok(objs
                    .into_iter()
                    .map(|f| AggroValue::Object(f))
                    .collect::<Vec<_>>())
            }
        }
    }
}

#[derive(Clone)]
pub enum LookAheadFields {
    Terminal(BTreeMap<Text, PyObject>),
    Nested(BTreeMap<Text, PyObject>, BTreeMap<Text, LookAheadFields>),
}

impl LookAheadFields {
    /// Extract python arguments from lookahead.
    pub fn arguments(&self) -> &BTreeMap<Text, PyObject> {
        match self {
            Terminal(args) => args,
            Nested(args, _) => args,
        }
    }
}

pub fn selection_to_fields(
    py: Python,
    field: &AggroField,
    c: &LookAheadSelection<AggroScalarValue>,
    all_inputs: &Arc<BTreeMap<Text, AggroObject>>,
    all_objects: &Arc<BTreeMap<Text, AggroObject>>,
) -> PuffResult<LookAheadFields> {
    let args = collect_arguments_for_python(py, all_inputs, field, c.arguments())?;
    if c.has_children() {
        let t = match &field.return_type.type_info {
            AggroTypeInfo::Object(t) => t,
            AggroTypeInfo::List(inner) => match &inner.type_info {
                AggroTypeInfo::Object(t) => t,
                _ => {
                    bail!("Input with children passed an object when none was expected.",)
                }
            },
            _ => {
                bail!("Input with children passed an object when none was expected.",)
            }
        };

        if let Some(obj) = all_objects.get(t) {
            let children = c.children();
            let mut final_res = BTreeMap::new();
            for child in children {
                let child_text_name = child.field_name().to_text();
                if let Some(f) = obj.fields.get(&child_text_name) {
                    final_res.insert(
                        child_text_name,
                        selection_to_fields(py, f, child, all_inputs, all_objects)?,
                    );
                }
            }
            Ok(Nested(args, final_res))
        } else {
            bail!("Couldn't find type {}", t)
        }
    } else {
        Ok(Terminal(args))
    }
}

fn collect_arguments_for_python(
    py: Python,
    all_inputs: &Arc<BTreeMap<Text, AggroObject>>,
    field: &AggroField,
    args: &[LookAheadArgument<AggroScalarValue>],
) -> PuffResult<BTreeMap<Text, PyObject>> {
    let mut ret = BTreeMap::new();
    for c in args {
        let key = c.name().to_text();
        if let Some(arg) = field.arguments.get(&key) {
            ret.insert(
                key,
                input_to_python(py, &arg.param_type, all_inputs, c.value())?,
            );
        }
    }
    Ok(ret)
}

#[pyclass]
pub struct SubscriptionSender {
    sender: UnboundedSender<Result<Value<AggroScalarValue>, ExecutionError<AggroScalarValue>>>,
    look_ahead: LookAheadFields,
    field: AggroField,
    all_objs: Arc<BTreeMap<Text, AggroObject>>,
    python_context: PyContext,
    rows: Arc<dyn ExtractValues + Send + Sync>,
}

impl SubscriptionSender {
    pub fn new(
        sender: UnboundedSender<Result<Value<AggroScalarValue>, ExecutionError<AggroScalarValue>>>,
        look_ahead: LookAheadFields,
        field: AggroField,
        all_objs: Arc<BTreeMap<Text, AggroObject>>,
        python_context: PyContext,
        rows: Arc<dyn ExtractValues + Send + Sync>,
    ) -> Self {
        Self {
            sender,
            look_ahead,
            field,
            all_objs,
            python_context,
            rows,
        }
    }
}

#[pymethods]
impl SubscriptionSender {
    fn __call__(&self, py: Python, ret_func: PyObject, new_function: PyObject) {
        let ctx = with_puff_context(|ctx| ctx);
        let postgres = ctx.postgres();
        let this_lookahead = self.look_ahead.clone();
        let this_field = self.field.clone();
        let all_objects = self.all_objs.clone();
        let python_context = self.python_context.clone();
        let this_sender = self.sender.clone();
        let rows = self.rows.clone();
        greenlet_async(ret_func, async move {
            let mut my_field = this_field;
            my_field.producer_method = Some(new_function);
            let pool = postgres.pool();
            let mut conn = pool.get().await?;
            let txn = conn.transaction().await?;
            let res = returned_values_into_stream(
                &txn,
                rows,
                &this_lookahead,
                &my_field,
                all_objects,
                python_context,
            )
            .await?;
            for r in res {
                if !this_sender.send(Ok(r)).is_ok() {
                    txn.rollback().await?;
                    return Err(anyhow!("Subscription websocket has closed."));
                }
            }
            txn.rollback().await?;
            Ok(true)
        })
    }
}
//
//
// pub fn run_subscription_method<'a>(
//     ctx: PuffContext,
//     client: &'a Client,
//     py_method: &'a PyObject,
//     aggro_field: &'a AggroField,
//     all_objs: &'a BTreeMap<String, AggroObject>,
//     look_ahead: &'a LookAheadSelection<AggroScalarValue>,
//     parents: Arc<dyn RowReturn + Sync + Send>,
// ) -> Pin<Box<dyn 'a + Send + Future<Output = Result<ValuesStream<AggroScalarValue>>>>> {
//     Box::pin(async move {
//         let maybe_aggro_obj = match &aggro_field.return_type.type_info {
//             AggroTypeInfo::List(b) => match &b.type_info {
//                 AggroTypeInfo::Object(inner) => all_objs.get(inner.as_str()),
//                 _ => None,
//             },
//             AggroTypeInfo::Object(tn) => all_objs.get(tn.as_str()),
//             _ => None,
//         };
//         let aggro_obj = if let Some(aggro_obj) = maybe_aggro_obj {
//             aggro_obj
//         } else {
//             return run_method_subscription_generator(ctx, py_method, look_ahead, parents).await;
//         };
//         let look_ahead_children = children_to_map(look_ahead);
//         let just_one = !aggro_value_is_list;
//
//         let (returned_values, maybe_assoc): (ReturnedValues, Option<String>) =
//             run_class_method(py_method, look_ahead_children, parents, just_one)?;
//
//         let result_vector: Arc<dyn RowReturn + Sync + Send> =
//             process_returned_values(client, returned_values, aggro_field, maybe_assoc, just_one)
//                 .await?;
//
//         let mut direct_columns = Vec::new();
//         for (f_n, c) in aggro_obj.fields.iter() {
//             if c.db_column.is_some() && look_ahead_children.contains_key(f_n.as_str()) {
//                 direct_columns.push(f_n.as_str())
//             }
//         }
//
//         let mut method_futures = Vec::new();
//         let mut used_methods = Vec::new();
//         let mut needed_fieds = Vec::new();
//         let class_methods = aggro_obj
//             .fields
//             .iter()
//             .filter(|(_s, v)| v.class_method.is_some());
//
//         for (field_n, child) in class_methods {
//             if let Some(s) = look_ahead_children.get(field_n.as_str()) {
//                 let r = result_vector.clone();
//                 let method_vals = run_method(
//                     client,
//                     child.class_method.as_ref().unwrap(),
//                     &child,
//                     all_objs,
//                     s,
//                     r,
//                 );
//                 used_methods.push((child.parent_field.as_ref(), field_n.as_str()));
//                 method_futures.push(method_vals);
//             }
//         }
//         let children_ret = try_join_all(method_futures.into_iter()).await?;
//         let method_iterators: Vec<(_, Option<usize>, BTreeMap<AggroScalarValue, AggroValue>)> =
//             used_methods
//                 .into_iter()
//                 .zip(children_ret)
//                 .map(|((parent_field, n), v)| {
//                     let pos = needed_fieds.len();
//                     if let Some(p) = parent_field {
//                         needed_fieds.push(p);
//                         (n, Some(pos), v)
//                     } else {
//                         (n, None, v)
//                     }
//                 })
//                 .collect();
//
//         let all_columns: Vec<_> = direct_columns
//             .clone()
//             .into_iter()
//             .chain(needed_fieds.iter().map(|s| s.as_str()))
//             .collect();
//
//         process_one_or_many(
//             result_vector,
//             method_iterators,
//             &all_columns,
//             &direct_columns,
//             look_ahead_children,
//         )
//     })
// }
