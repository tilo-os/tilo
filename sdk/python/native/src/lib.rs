//! `tilo._native`: Tilo's API in process for the Python SDK, as a key the
//! program holds ([`tilo_api::direct`]), so `AsyncTilo.host(...)` reaches a
//! Tilo host directly as a device of the person, with no API server between.
//!
//! Every request runs on one tokio runtime for the process and is awaited
//! from asyncio without holding the GIL; a streamed answer (server-sent
//! events) is an async iterator of its chunks. The Automerge bytes the host
//! sends are decoded in sandboxed workers, started as the
//! `tilo-automerge-worker` program the wheel carries beside this module.

use pyo3::exceptions::{PyImportError, PyStopAsyncIteration, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyString};
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;
use tilo_api::direct::{AnswerBody, Direct, KeyFile};

/// A Tilo host reached directly with the key a key file holds.
#[pyclass(frozen, module = "tilo._native")]
struct Host {
    direct: Arc<Direct>,
}

#[pymethods]
impl Host {
    /// `key_file`: the path of a key file, or its contents as a dict
    /// (`{"host", "name", "key", "cert"}`).
    #[new]
    fn new(key_file: &Bound<'_, PyAny>) -> PyResult<Self> {
        let file = key_file_of(key_file)?;
        serve_workers_beside_module(key_file.py())?;
        let _runtime = pyo3_async_runtimes::tokio::get_runtime().enter();
        let direct = Direct::new(&file).map_err(PyValueError::new_err)?;
        Ok(Self {
            direct: Arc::new(direct),
        })
    }

    /// Answer `method` on `path` (with its query) as the API does: awaits
    /// `(status, headers, body)`, the body `bytes`, or an async iterator of
    /// `bytes` chunks for a `text/event-stream` answer.
    fn request<'py>(
        &self,
        py: Python<'py>,
        method: String,
        path: String,
        headers: HashMap<String, String>,
        body: &[u8],
    ) -> PyResult<Bound<'py, PyAny>> {
        let direct = Arc::clone(&self.direct);
        let headers: Vec<(String, String)> = headers.into_iter().collect();
        let body = body.to_vec();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let answer = direct.answer(&method, &path, &headers, &body).await;
            let body = match answer.body {
                AnswerBody::Whole(bytes) => Body::Whole(Cow::Owned(bytes.to_vec())),
                AnswerBody::Stream(chunks) => Body::Stream(Chunks {
                    chunks: Arc::new(chunks),
                }),
            };
            let headers: HashMap<String, String> = answer.headers.into_iter().collect();
            Ok((answer.status, headers, body))
        })
    }
}

/// An answer's body as Python gets it.
#[derive(IntoPyObject)]
enum Body {
    Whole(Cow<'static, [u8]>),
    Stream(Chunks),
}

/// A streamed answer's chunks, as an async iterator of `bytes`.
#[pyclass(frozen, module = "tilo._native")]
struct Chunks {
    chunks: Arc<tilo_api::direct::Chunks>,
}

#[pymethods]
impl Chunks {
    const fn __aiter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __anext__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let chunks = Arc::clone(&self.chunks);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            match chunks.next().await {
                Some(chunk) => Ok(Cow::<[u8]>::Owned(chunk.to_vec())),
                None => Err(PyStopAsyncIteration::new_err(())),
            }
        })
    }
}

/// Start the client's Automerge workers as the `tilo-automerge-worker`
/// program the wheel carries beside this module: Python cannot be started
/// again as one.
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

/// The key file `value` names (a path) or is (a dict).
fn key_file_of(value: &Bound<'_, PyAny>) -> PyResult<KeyFile> {
    if let Ok(path) = value.cast::<PyString>() {
        let path = path.to_cow()?;
        return KeyFile::read(std::path::Path::new(path.as_ref())).map_err(PyValueError::new_err);
    }
    if value.cast::<PyDict>().is_ok() {
        let text: String = value
            .py()
            .import("json")?
            .call_method1("dumps", (value,))?
            .extract()?;
        return serde_json::from_str(&text)
            .map_err(|error| PyValueError::new_err(format!("not a key file: {error}")));
    }
    Err(PyTypeError::new_err(
        "a key file is its path or a dict of its contents",
    ))
}

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<Host>()?;
    module.add_class::<Chunks>()?;
    Ok(())
}
