//! `tilo._native`: the Python SDK's way into Tilo, the device daemon's native endpoint.
//!
//! A program connects to this computer's daemon as `tilo`'s workspace commands do
//! ([`tilo_api::calls::Calls`]): action frames ([`tilo_actions::remote::RemoteActionFrame`]),
//! one per line, carried by the line wire's one client, walked as the agent surface's tools in
//! this process. The daemon admits the connection as whoever makes it: the person's own
//! account, or an app process the daemon started, in the one workspace it was started in.
//!
//! Every call runs on one tokio runtime for the process and is awaited from asyncio without
//! holding the GIL; it answers `{"answer": ...}` or `{"refused": {"error": {...}}}` as JSON text,
//! the refusal's body as the SDK raises its typed errors from. The Automerge bytes a call reads
//! are decoded in sandboxed workers, started as the `tilo-automerge-worker` program the wheel
//! carries beside this module.

use pyo3::exceptions::{PyConnectionError, PyImportError, PyValueError};
use pyo3::prelude::*;
use std::sync::Arc;
use tilo_api::calls::Calls;

/// A connection to this computer's daemon.
#[pyclass(frozen, module = "tilo._native")]
struct Daemon {
    calls: Arc<Calls>,
}

#[pymethods]
impl Daemon {
    /// The workspace this connection acts in when it names none.
    #[getter]
    fn workspace(&self) -> Option<String> {
        self.calls.served_workspace().map(str::to_owned)
    }

    /// Call `tool` with `args` (JSON text) in `workspace` (`None`: the account's own tree), as a
    /// step of `group` (JSON text of `{id, title}`) when one is named: awaits JSON text of
    /// `{"answer": ...}` or `{"refused": {"error": {...}}}`.
    #[pyo3(signature = (workspace, tool, args, group=None))]
    fn call<'py>(
        &self,
        py: Python<'py>,
        workspace: Option<String>,
        tool: String,
        args: &str,
        group: Option<&str>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let args: serde_json::Value = serde_json::from_str(args)
            .map_err(|error| PyValueError::new_err(format!("the arguments: {error}")))?;
        let group: Option<serde_json::Value> = group
            .map(serde_json::from_str)
            .transpose()
            .map_err(|error| PyValueError::new_err(format!("the group: {error}")))?;
        let calls = Arc::clone(&self.calls);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let answer = match calls.call(workspace.as_deref(), &tool, args, group).await {
                Ok(answer) => serde_json::json!({ "answer": answer }),
                Err(refused) => serde_json::json!({ "refused": refused }),
            };
            Ok(answer.to_string())
        })
    }

    /// The secret of the vault item `item` in `workspace` (`None`: the one this connection acts
    /// in), released to this process ([`Calls::release`]): awaits JSON text of
    /// `{"answer": {"secret": ...}}` or `{"refused": {"error": {...}}}`. The secret goes to the
    /// caller alone.
    fn release<'py>(
        &self,
        py: Python<'py>,
        workspace: Option<String>,
        item: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let calls = Arc::clone(&self.calls);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let answer = match calls.release(workspace.as_deref(), &item).await {
                Ok(answer) => serde_json::json!({ "answer": answer }),
                Err(refused) => serde_json::json!({ "refused": refused }),
            };
            Ok(answer.to_string())
        })
    }
}

/// Connect to the daemon's native endpoint ([`Calls::connect`]): the connection the launcher
/// handed an app it started, else `socket`, else this computer's daemon. `workspace` names the one
/// workspace the connection acts in when it knows it. Awaits the [`Daemon`].
#[pyfunction]
#[pyo3(signature = (socket=None, workspace=None))]
fn connect(
    py: Python<'_>,
    socket: Option<String>,
    workspace: Option<String>,
) -> PyResult<Bound<'_, PyAny>> {
    serve_workers_beside_module(py)?;
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let socket = socket.map(std::path::PathBuf::from);
        let calls = Calls::connect(socket.as_deref(), workspace.as_deref())
            .await
            .map_err(|error| PyConnectionError::new_err(format!("{error:#}")))?;
        Ok(Daemon {
            calls: Arc::new(calls),
        })
    })
}

/// Start the client's Automerge workers as the `tilo-automerge-worker` program the wheel
/// carries beside this module: Python cannot be started again as one.
fn serve_workers_beside_module(py: Python<'_>) -> PyResult<()> {
    let module: std::path::PathBuf = py.import("tilo._native")?.getattr("__file__")?.extract()?;
    let worker = module.with_file_name("tilo-automerge-worker");
    if !worker.is_file() {
        return Err(PyImportError::new_err(format!(
            "tilo._native is incomplete: {} is missing",
            worker.display()
        )));
    }
    tilo_automerge_isolate::serve_workers_from(worker);
    Ok(())
}

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<Daemon>()?;
    module.add_function(wrap_pyfunction!(connect, module)?)?;
    Ok(())
}
