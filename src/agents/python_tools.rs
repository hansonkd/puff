use crate::agents::error::AgentError;
use crate::agents::llm::ToolDefinition;
use crate::agents::tool::{RegisteredTool, ToolExecutor};
use crate::context::with_puff_context;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct PythonToolMetadata {
    name: String,
    #[serde(default)]
    description: String,
    input_schema: serde_json::Value,
    #[serde(default)]
    requires_approval: bool,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
}

const fn default_timeout_ms() -> u64 {
    30_000
}

pub fn load_python_tools_module(module: &str) -> Result<Vec<RegisteredTool>, AgentError> {
    Python::with_gil(|py| -> Result<Vec<RegisteredTool>, AgentError> {
        let inspect = py
            .import("inspect")
            .map_err(|error| python_tool_load_error(module, error))?;
        let tool_module = py
            .import(module)
            .map_err(|error| python_tool_load_error(module, error))?;
        let mut tools = Vec::new();

        for (_, value) in tool_module.dict().iter() {
            let is_function = inspect
                .call_method1("isfunction", (&value,))
                .map_err(|error| python_tool_load_error(module, error))?
                .extract::<bool>()
                .map_err(|error| python_tool_load_error(module, error))?;
            let is_coroutine = inspect
                .call_method1("iscoroutinefunction", (&value,))
                .map_err(|error| python_tool_load_error(module, error))?
                .extract::<bool>()
                .map_err(|error| python_tool_load_error(module, error))?;
            if !(is_function || is_coroutine) {
                continue;
            }

            let Ok(metadata_obj) = value.getattr("__puff_tool__") else {
                continue;
            };
            let metadata: PythonToolMetadata =
                pythonize::depythonize(&metadata_obj).map_err(|error| {
                    AgentError::SkillLoadError {
                        skill: module.to_string(),
                        message: format!("invalid Python tool metadata: {error}"),
                    }
                })?;
            let function = value
                .getattr("__name__")
                .map_err(|error| python_tool_load_error(module, error))?
                .extract::<String>()
                .map_err(|error| python_tool_load_error(module, error))?;

            tools.push(RegisteredTool {
                definition: ToolDefinition {
                    name: metadata.name,
                    description: metadata.description,
                    input_schema: metadata.input_schema,
                },
                executor: ToolExecutor::Python {
                    module: module.to_string(),
                    function,
                },
                requires_approval: metadata.requires_approval,
                timeout_ms: metadata.timeout_ms,
            });
        }

        tools.sort_by(|left, right| left.definition.name.cmp(&right.definition.name));
        Ok(tools)
    })
}

pub async fn execute_python_tool(
    module: &str,
    function: &str,
    arguments: &serde_json::Value,
) -> Result<String, AgentError> {
    let dispatcher = with_puff_context(|ctx| ctx.python_dispatcher());
    let (python_function, is_coroutine, kwargs) = Python::with_gil(|py| -> PyResult<_> {
        let puff = py.import("puff")?;
        let inspect = py.import("inspect")?;
        let function_obj = puff.call_method1("cached_import", (module, function))?;
        let is_coroutine = inspect
            .call_method1("iscoroutinefunction", (&function_obj,))?
            .extract::<bool>()?;
        let kwargs = match arguments {
            serde_json::Value::Null => PyDict::new(py).unbind(),
            serde_json::Value::Object(_) => pythonize::pythonize(py, arguments)?
                .downcast_into::<PyDict>()
                .map_err(|_| {
                    pyo3::exceptions::PyTypeError::new_err(
                        "tool arguments must serialize to a Python dict",
                    )
                })?
                .unbind(),
            _ => {
                return Err(pyo3::exceptions::PyTypeError::new_err(
                    "tool arguments must be a JSON object",
                ));
            }
        };
        Ok((function_obj.unbind(), is_coroutine, kwargs))
    })
    .map_err(|error| AgentError::ToolExecutionError {
        tool: format!("{module}.{function}"),
        message: error.to_string(),
    })?;

    let receiver = if is_coroutine {
        dispatcher
            .dispatch_asyncio(python_function, (), Some(kwargs))
            .map_err(|error| AgentError::ToolExecutionError {
                tool: format!("{module}.{function}"),
                message: error.to_string(),
            })?
    } else {
        dispatcher
            .dispatch(python_function, (), Some(kwargs))
            .map_err(|error| AgentError::ToolExecutionError {
                tool: format!("{module}.{function}"),
                message: error.to_string(),
            })?
    };

    let result = receiver
        .await
        .map_err(|error| AgentError::ToolExecutionError {
            tool: format!("{module}.{function}"),
            message: format!("Python tool response channel failed: {error}"),
        })?;
    let result = result.map_err(|error| AgentError::ToolExecutionError {
        tool: format!("{module}.{function}"),
        message: error.to_string(),
    })?;

    Python::with_gil(|py| -> Result<String, AgentError> {
        let bound = result.bind(py);
        if let Ok(json_value) = pythonize::depythonize::<serde_json::Value>(bound) {
            return match json_value {
                serde_json::Value::String(text) => Ok(text),
                other => {
                    serde_json::to_string(&other).map_err(|error| AgentError::ToolExecutionError {
                        tool: format!("{module}.{function}"),
                        message: format!("failed to serialize Python tool result: {error}"),
                    })
                }
            };
        }

        bound
            .str()
            .map_err(|error| AgentError::ToolExecutionError {
                tool: format!("{module}.{function}"),
                message: error.to_string(),
            })?
            .to_str()
            .map(|text| text.to_string())
            .map_err(|error| AgentError::ToolExecutionError {
                tool: format!("{module}.{function}"),
                message: error.to_string(),
            })
    })
}

fn python_tool_load_error(module: &str, error: PyErr) -> AgentError {
    AgentError::SkillLoadError {
        skill: module.to_string(),
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_python_tools_module_reads_decorated_functions() {
        pyo3::prepare_freethreaded_python();
        Python::with_gil(|py| -> PyResult<()> {
            let sys = py.import("sys")?;
            let types = py.import("types")?;
            sys.getattr("path")?
                .call_method1("insert", (0, "/root/puff/puff_py"))?;

            let module = types.getattr("ModuleType")?.call1(("puff_test_tools",))?;
            let globals = module.getattr("__dict__")?.downcast_into::<PyDict>()?;
            py.run(
                pyo3::ffi::c_str!(
                    r#"
from puff import tool

@tool
def hello(name: str) -> dict:
    """Say hello."""
    return {"hello": name}
"#
                ),
                None,
                Some(&globals),
            )?;
            sys.getattr("modules")?
                .set_item("puff_test_tools", module)?;
            Ok(())
        })
        .expect("python module");

        let tools = load_python_tools_module("puff_test_tools").expect("tools");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].definition.name, "hello");
        assert_eq!(tools[0].definition.description, "Say hello.");
        assert_eq!(
            tools[0].definition.input_schema["properties"]["name"]["type"],
            "string"
        );
        assert!(matches!(tools[0].executor, ToolExecutor::Python { .. }));
    }
}
