//! The agent's keyless web tools (`search_web` and `read_page`), as their own
//! library in the runtime bundle (`libtilo_agent_web_engine.so`).
//!
//! Like `runtime-dylib` it re-exports what it provides, which records it
//! as provided by this library for the apps' `-C prefer-dynamic` link: the
//! agent names the tools through it and carries none of their code.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

#[cfg(feature = "thin")]
pub use tilo_agent_web_tools;

#[cfg(feature = "thin")]
pub use tilo_web_search;

/// Run the library's own code: build the client every request goes through,
/// reduce a page to readable text, refuse a page on this machine, and answer
/// from the runtime the agent waits on.
///
/// The engine's check binary and the agent's `--probe-agent-web-engine` call
/// it, so a release proves the library loads and runs against its runtime.
#[cfg(feature = "thin")]
pub fn self_check() -> Result<String, String> {
    use tilo_web_search::readable;

    tilo_web_search::client().map_err(|error| format!("the HTTP client did not build: {error}"))?;
    let base = url::Url::parse("https://example.test/")
        .map_err(|error| format!("no base url: {error}"))?;
    let page = readable::extract(
        "<html><head><title>A page</title></head><body><main><h1>Heading</h1>\
         <p>First paragraph.</p><p>Second paragraph.</p></main></body></html>",
        &base,
    );
    if page.title.as_deref() != Some("A page") {
        return Err(format!("the page's title is {:?}", page.title));
    }
    for want in ["Heading", "First paragraph.", "Second paragraph."] {
        if !page.text.contains(want) {
            return Err(format!("the readable text lacks {want:?}: {:?}", page.text));
        }
    }
    let local =
        url::Url::parse("http://127.0.0.1/").map_err(|error| format!("no local url: {error}"))?;
    if tilo_web_search::check_url(&local).is_ok() {
        return Err("a page on this machine is not refused".to_string());
    }
    let runtime = tilo_rt::daemon("agent-web-engine-check")
        .workers(1)
        .build()
        .map_err(|error| format!("the runtime did not start: {error}"))?;
    let answer = tilo_rt::wait_on(runtime.handle(), async {
        tilo_rt::sleep(std::time::Duration::from_millis(1)).await;
        40 + 2
    })
    .map_err(|error| format!("the runtime did not answer: {error}"))?;
    if answer != 42 {
        return Err(format!("the runtime answered {answer}, not 42"));
    }
    Ok(format!(
        "the client built and {} characters of readable text, the local page refused, the runtime answered",
        page.text.chars().count()
    ))
}
