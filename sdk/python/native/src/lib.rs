//! `tilo._native`: the Python SDK's way into Tilo, the device daemon's native endpoint.
//!
//! A program connects to this computer's daemon as `tilo`'s workspace commands do
//! ([`tilo_api::calls::Calls`]), over the line wire's one client. The daemon admits the
//! connection as whoever makes it: the person's own account, or an app process the daemon
//! started, in the one workspace it was started in. Every request of the API (`/v1`) on it is
//! answered by the daemon as that connection is admitted ([`Calls::request`]); its body streams
//! back in pieces, as a stream of events goes on sending. The vault and the call lane are the
//! app's connection's own, answered as JSON text.
//!
//! The Automerge bytes a call reads are decoded in sandboxed workers, started as the
//! `tilo-automerge-worker` program the wheel carries beside this module.

use pyo3::exceptions::{PyConnectionError, PyImportError, PyValueError};
use pyo3::prelude::*;
use std::sync::Arc;
use tilo_api::calls::{ApiAnswer, Calls};

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

    /// Have the daemon answer `method` on `target` (a `/v1` path with its query) with `headers`
    /// and `body`, as its API answers this connection: an app with its own grants, never its
    /// person's. Awaits the [`Reply`]; raises `ConnectionError` when the daemon did not answer.
    /// A refusal is an answer, with its status.
    #[pyo3(signature = (method, target, headers, body=None))]
    fn request<'py>(
        &self,
        py: Python<'py>,
        method: String,
        target: String,
        headers: Vec<Vec<String>>,
        body: Option<Vec<u8>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let headers = headers
            .into_iter()
            .filter_map(|pair| match <[String; 2]>::try_from(pair) {
                Ok([name, value]) => Some((name, value)),
                Err(_) => None,
            })
            .collect();
        let body = body.unwrap_or_default();
        let calls = Arc::clone(&self.calls);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let answer = calls
                .request(&method, &target, headers, body)
                .await
                .map_err(PyConnectionError::new_err)?;
            Ok(Reply {
                status: answer.status,
                headers: answer
                    .headers
                    .iter()
                    .map(|(name, value)| vec![name.clone(), value.clone()])
                    .collect(),
                body: Arc::new(answer),
            })
        })
    }

    /// Make the web call `args` (JSON text) names with the key of the vault item it names, in
    /// `workspace` (`None`: the one this connection acts in): awaits JSON text of
    /// `{"answer": ...}` or `{"refused": {"error": {...}}}`. An app's own connection has the
    /// vault add the key to the request; the secret never reaches the app.
    #[pyo3(signature = (workspace, args))]
    fn web_call<'py>(
        &self,
        py: Python<'py>,
        workspace: Option<String>,
        args: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let arguments: serde_json::Map<String, serde_json::Value> = serde_json::from_str(args)
            .map_err(|error| PyValueError::new_err(format!("the arguments: {error}")))?;
        let calls = Arc::clone(&self.calls);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let answer = match calls.web_call(workspace.as_deref(), arguments).await {
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

    /// The reference of the vault item tagged for `app` and `purpose` in `workspace` (`None`:
    /// the one this connection acts in), found on this app's own connection ([`Calls::find`]):
    /// awaits JSON text of `{"answer": {"reference": ...}}` or `{"answer": {}}`, or
    /// `{"refused": {"error": {...}}}`. It releases nothing.
    fn find<'py>(
        &self,
        py: Python<'py>,
        workspace: Option<String>,
        app: String,
        purpose: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let calls = Arc::clone(&self.calls);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let answer = match calls.find(workspace.as_deref(), &app, &purpose).await {
                Ok(answer) => serde_json::json!({ "answer": answer }),
                Err(refused) => serde_json::json!({ "refused": refused }),
            };
            Ok(answer.to_string())
        })
    }

    /// Carry the outside call `pair` (JSON text of `{outside, tilo}`, or `null`: let go of the
    /// one carried) into the workspace call, on this app's connection: awaits JSON text of
    /// `{"answer": {}}` or `{"refused": {"error": {...}}}`. The first carry joins the call as the
    /// app, which the workspace's grants decide.
    fn carry_call<'py>(&self, py: Python<'py>, pair: &str) -> PyResult<Bound<'py, PyAny>> {
        let pair: serde_json::Value = serde_json::from_str(pair)
            .map_err(|error| PyValueError::new_err(format!("the pair: {error}")))?;
        let calls = Arc::clone(&self.calls);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            Ok(answered(calls.carry_call(pair).await).to_string())
        })
    }

    /// Say who is in the outside calls and where the app stands in them (JSON text of `{calls,
    /// link, status}`): awaits JSON text of `{"answer": {}}` or `{"refused": {"error": {...}}}`.
    fn report_call<'py>(&self, py: Python<'py>, report: &str) -> PyResult<Bound<'py, PyAny>> {
        let report: serde_json::Value = serde_json::from_str(report)
            .map_err(|error| PyValueError::new_err(format!("the report: {error}")))?;
        let calls = Arc::clone(&self.calls);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            Ok(answered(calls.report_call(report).await).to_string())
        })
    }

    /// Hand the workspace's call one Opus frame `speaker`, an outside account, said: `None`, or
    /// JSON text of the refusal's body. Nothing waits for an answer, as nothing waits in a call.
    fn hear_call(&self, speaker: String, opus: Vec<u8>) -> Option<String> {
        self.calls
            .hear_call(speaker, opus)
            .err()
            .map(|refused| refused.to_string())
    }

    /// What the workspace's call said since the last time, and where the device asks the app to
    /// be, when that is new: JSON text of `{"answer": {"frames": [{speaker, opus, skipped}],
    /// "asked": {call, status} | null}}`, `opus` as a list of bytes, or `{"refused": ...}`.
    fn said_call(&self) -> String {
        let said = self.calls.said_call().map(|said| {
            serde_json::json!({
                "frames": said.frames,
                "asked": said
                    .asked
                    .map(|(call, status)| serde_json::json!({ "call": call, "status": status })),
            })
        });
        answered(said).to_string()
    }
}

/// What the daemon's API answered one request with: its status and headers, and its body as it
/// comes.
#[pyclass(frozen, module = "tilo._native")]
struct Reply {
    status: u16,
    headers: Vec<Vec<String>>,
    body: Arc<ApiAnswer>,
}

#[pymethods]
impl Reply {
    #[getter]
    fn status(&self) -> u16 {
        self.status
    }

    /// Each header as `[name, value]`.
    #[getter]
    fn headers(&self) -> Vec<Vec<String>> {
        self.headers.clone()
    }

    /// The next piece of the body; `None` once it ended. Raises `ConnectionError` when the
    /// connection closed first.
    fn next<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let body = Arc::clone(&self.body);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            body.next().await.map_err(PyConnectionError::new_err)
        })
    }
}

/// A call's outcome as the SDK reads it: `{"answer": ...}` or `{"refused": ...}`.
fn answered(outcome: Result<serde_json::Value, serde_json::Value>) -> serde_json::Value {
    match outcome {
        Ok(answer) => serde_json::json!({ "answer": answer }),
        Err(refused) => serde_json::json!({ "refused": refused }),
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
    module.add_class::<Reply>()?;
    module.add_function(wrap_pyfunction!(connect, module)?)?;
    Ok(())
}
