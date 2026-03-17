use pyo3::prelude::*;

#[pyclass(name = "Agent")]
pub struct PyAgent {
    pub name: String,
    pub model: String,
    pub system_prompt: Option<String>,
}

#[pymethods]
impl PyAgent {
    #[new]
    #[pyo3(signature = (name, model="claude-sonnet-4-6", system_prompt=None, skills=None, tools=None, memory=None, permissions=None))]
    fn new(
        name: String,
        model: &str,
        system_prompt: Option<String>,
        skills: Option<Vec<String>>,
        tools: Option<Vec<PyObject>>,
        memory: Option<PyObject>,
        permissions: Option<PyObject>,
    ) -> Self {
        let _ = (skills, tools, memory, permissions);
        Self {
            name,
            model: model.to_string(),
            system_prompt,
        }
    }

    #[getter]
    fn name(&self) -> &str {
        &self.name
    }

    #[getter]
    fn model(&self) -> &str {
        &self.model
    }

    #[getter]
    fn system_prompt(&self) -> Option<&str> {
        self.system_prompt.as_deref()
    }
}

#[pyclass(name = "ToolDef")]
pub struct PyToolDef {
    pub name: String,
    pub description: String,
    pub function: PyObject,
}

#[pymethods]
impl PyToolDef {
    #[new]
    fn new(name: String, description: String, function: PyObject) -> Self {
        Self {
            name,
            description,
            function,
        }
    }

    #[getter]
    fn name(&self) -> &str {
        &self.name
    }

    #[getter]
    fn description(&self) -> &str {
        &self.description
    }
}

pub fn register_python_classes(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyAgent>()?;
    m.add_class::<PyToolDef>()?;
    Ok(())
}
