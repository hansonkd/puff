use std::collections::HashMap;

use axum::{extract, Json};
use puff_rs::prelude::*;
use puff_rs::program::commands::ASGIServerCommand;
use serde_json::Map;

// Use pyo3 to generate Python compatible Rust classes.
#[pyclass]
struct MyPythonState;

#[pymethods]
impl MyPythonState {
    // Async Puff functions take a function to return the result with and offload the future onto Tokio.
    fn hello_from_rust_async(&self, return_func: PyObject, py_says: Text) {
        run_python_async(return_func, async move {
            // tokio::time::sleep(Duration::from_secs(1)).await;
            debug!("Python says: {}", &py_says);
            Ok(42)
        })
    }
}

fn main() -> ExitCode {
    let app = Router::new().get("/", root).post("/", root_post);
    let rc = RuntimeConfig::default()
        .add_python_path("./examples")
        .set_asyncio(true)
        .set_global_state_fn(|py| Ok(MyPythonState.into_py(py)));

    Program::new("my_first_app")
        .about("This is my first app")
        .runtime_config(rc)
        .command(ASGIServerCommand::new_with_router(
            "fast_api_example.app",
            app,
        ))
        .run()
}

// Basic handler that responds with a static string
async fn root() -> Text {
    "Ok".to_text()
}

// Basic handler that transforms json sent to it.
async fn root_post(
    extract::Json(payload): extract::Json<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let mut ret_map = Map::new();
    for (k, v) in payload {
        ret_map.insert(
            format!("Key was {}", k),
            serde_json::Value::String(format!("Value was {}", v.to_uppercase())),
        );
    }
    Json(serde_json::Value::Object(ret_map))
}
