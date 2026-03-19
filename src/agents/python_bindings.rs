use pyo3::prelude::*;

/// An AI agent with tools, memory, and orchestration capabilities.
///
/// Args:
///     name: Agent name (required)
///     model: LLM model to use (default: "claude-sonnet-4-6")
///     system_prompt: System prompt for the agent
///     skills: List of skill directory paths (not yet wired — use puff.toml)
///     tools: List of tool objects (not yet wired — use puff.toml)
///     memory: Memory configuration (not yet wired — use puff.toml)
///     permissions: Permission configuration (not yet wired — use puff.toml)
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
        if skills.is_some() || tools.is_some() || memory.is_some() || permissions.is_some() {
            tracing::warn!(
                "Agent '{}': skills/tools/memory/permissions params not yet wired to Rust runtime. Use puff.toml configuration instead.",
                name
            );
        }
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

/// A tool definition for use with Puff agents.
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
