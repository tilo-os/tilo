//! `tilo-app run --target web`: serve the bundle, rebuild on change, reload
//! the page.
//!
//! A deliberately small HTTP/1.1 server on loopback: `GET` of bundle files
//! with `no-store` caching, and `/__tilo/events`, a server-sent event stream
//! the served `index.html` subscribes to. A successful rebuild sends
//! `reload`; a failed one sends `error` and keeps the last good bundle
//! served, as the native `dev` loop keeps the last good app running.

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use notify::{Event, RecommendedWatcher};

use super::{Project, TargetRequest, web};

/// How `run --target web` serves.
#[derive(Clone, Debug)]
pub struct ServeOptions {
    pub address: SocketAddr,
    /// Rebuild and reload on source changes.
    pub watch: bool,
    pub debounce: Duration,
}

impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            address: SocketAddr::from(([127, 0, 0, 1], 7878)),
            watch: true,
            debounce: Duration::from_millis(150),
        }
    }
}

const EVENTS_PATH: &str = "/__tilo/events";
const RELOAD_SCRIPT: &str = "<script type=\"module\">\n  // Injected by `tilo-app run --target web`: reload after each successful rebuild.\n  const events = new EventSource('/__tilo/events');\n  events.addEventListener('reload', () => location.reload());\n  events.addEventListener('error-build', event => console.error('tilo-app build failed:\\n' + JSON.parse(event.data)));\n</script>\n";

struct Server {
    root: PathBuf,
    clients: Mutex<Vec<TcpStream>>,
}

impl Server {
    fn broadcast(&self, event: &str, data: &str) {
        let payload = format!("event: {event}\ndata: {data}\n\n");
        let mut clients = self.clients.lock().unwrap_or_else(PoisonError::into_inner);
        clients.retain_mut(|client| {
            client
                .write_all(payload.as_bytes())
                .and_then(|()| client.flush())
                .is_ok()
        });
    }
}

pub(super) fn run(
    project: &Project,
    request: &TargetRequest,
    options: &ServeOptions,
) -> Result<()> {
    let first = web::build(project, request)?;
    let listener = TcpListener::bind(options.address).with_context(|| {
        format!(
            "listening on {}\n  help: pass `--port` to use another port",
            options.address
        )
    })?;
    let server = Arc::new(Server {
        root: first.artifact.clone(),
        clients: Mutex::new(Vec::new()),
    });
    let accepting = Arc::clone(&server);
    std::thread::Builder::new()
        .name("tilo-app-serve".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let server = Arc::clone(&accepting);
                let _ = std::thread::Builder::new()
                    .name("tilo-app-request".into())
                    .spawn(move || {
                        if let Err(error) = handle(&server, stream) {
                            eprintln!("[tilo-app] request failed: {error:#}");
                        }
                    });
            }
        })
        .context("starting the web server")?;
    eprintln!(
        "[tilo-app] serving {} at http://{}/{}",
        project.display_name(),
        options.address,
        first.entry
    );
    if !options.watch {
        eprintln!("[tilo-app] press Ctrl-C to stop");
        loop {
            std::thread::park();
        }
    }
    watch(project, request, options, &server)
}

fn watch(
    project: &Project,
    request: &TargetRequest,
    options: &ServeOptions,
    server: &Server,
) -> Result<()> {
    let roots = crate::dev_watch_roots(&project.app_dir, true)?;
    let (event_tx, event_rx) = mpsc::channel::<notify::Result<Event>>();
    let watcher: RecommendedWatcher = notify::recommended_watcher(move |event| {
        let _ = event_tx.send(event);
    })
    .context("creating the source watcher")?;
    let mut watcher = crate::DevWatcher::new(watcher);
    for root in &roots {
        watcher.watch_tree(root)?;
    }
    let _signals = crate::DevSignalGuard::install()?;
    eprintln!(
        "[tilo-app] watching {}; the page reloads after each successful build",
        roots
            .iter()
            .map(|root| root.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    while !crate::DEV_STOP_REQUESTED.load(Ordering::Relaxed) {
        match event_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(event)) => {
                watcher.observe(&event);
                // The build links fonts and page modules out of the watched
                // SDK tree; that changes their metadata, not their contents.
                let metadata_only = matches!(
                    event.kind,
                    notify::EventKind::Modify(notify::event::ModifyKind::Metadata(_))
                );
                if metadata_only || !crate::event_requires_restart(&event, &roots) {
                    continue;
                }
                crate::drain_until_quiet(&event_rx, &roots, options.debounce, &mut watcher);
                eprintln!("[tilo-app] change detected; rebuilding the web bundle");
                match web::build(project, request) {
                    Ok(_) => {
                        server.broadcast("reload", "{}");
                    }
                    Err(error) => {
                        eprintln!(
                            "[tilo-app] build failed; keeping the last good bundle\n{error:#}"
                        );
                        let message =
                            serde_json::to_string(&format!("{error:#}")).unwrap_or_default();
                        server.broadcast("error-build", &message);
                    }
                }
            }
            Ok(Err(error)) => eprintln!("[tilo-app] watch error: {error}"),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => bail!("the source watcher stopped"),
        }
    }
    eprintln!("[tilo-app] stopped");
    Ok(())
}

fn handle(server: &Server, mut stream: TcpStream) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 || header == "\r\n" || header == "\n" {
            break;
        }
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/");
    if method != "GET" && method != "HEAD" {
        return respond(
            &mut stream,
            "405 Method Not Allowed",
            "text/plain",
            b"method not allowed\n",
            false,
        );
    }
    let path = target.split(['?', '#']).next().unwrap_or("/");
    if path == EVENTS_PATH {
        stream.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nConnection: keep-alive\r\n\r\n: connected\n\n",
        )?;
        stream.set_read_timeout(None)?;
        server
            .clients
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(stream);
        return Ok(());
    }
    let Some(file) = resolve(&server.root, path) else {
        return respond(
            &mut stream,
            "404 Not Found",
            "text/plain",
            b"not found\n",
            method == "HEAD",
        );
    };
    let Ok(mut body) = std::fs::read(&file) else {
        return respond(
            &mut stream,
            "404 Not Found",
            "text/plain",
            b"not found\n",
            method == "HEAD",
        );
    };
    let content_type = content_type(&file);
    if file.file_name().and_then(|name| name.to_str()) == Some("index.html") {
        body = inject_reload(&body);
    }
    respond(&mut stream, "200 OK", content_type, &body, method == "HEAD")
}

fn respond(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
    head: bool,
) -> Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    if !head {
        stream.write_all(body)?;
    }
    stream.flush()?;
    Ok(())
}

/// Map a request path into `root`, refusing anything that could leave it.
fn resolve(root: &Path, path: &str) -> Option<PathBuf> {
    let decoded = percent_decode(path)?;
    let relative = decoded.trim_start_matches('/');
    let relative = if relative.is_empty() {
        "index.html"
    } else {
        relative
    };
    let relative = Path::new(relative);
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return None;
    }
    let file = root.join(relative);
    if file.is_dir() {
        let index = file.join("index.html");
        return index.is_file().then_some(index);
    }
    file.is_file().then_some(file)
}

fn percent_decode(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while let Some(&byte) = bytes.get(index) {
        if byte == b'%' {
            let hex = bytes.get(index.checked_add(1)?..index.checked_add(3)?)?;
            let value = u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
            if value == b'/' || value == b'\\' || value == 0 {
                return None;
            }
            out.push(value);
            index = index.checked_add(3)?;
        } else {
            out.push(byte);
            index = index.checked_add(1)?;
        }
    }
    String::from_utf8(out).ok()
}

fn inject_reload(body: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(body);
    match text.rfind("</body>") {
        Some(at) => {
            let (head, tail) = text.split_at(at);
            format!("{head}{RELOAD_SCRIPT}{tail}").into_bytes()
        }
        None => format!("{text}{RELOAD_SCRIPT}").into_bytes(),
    }
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("json") => "application/json",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("woff2") => "font/woff2",
        Some("txt" | "toml" | "md") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_cannot_leave_the_bundle() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), "<body></body>").unwrap();
        std::fs::create_dir(dir.path().join("pkg")).unwrap();
        std::fs::write(dir.path().join("pkg/a.js"), "").unwrap();
        assert_eq!(
            resolve(dir.path(), "/"),
            Some(dir.path().join("index.html"))
        );
        assert_eq!(
            resolve(dir.path(), "/pkg/a.js"),
            Some(dir.path().join("pkg/a.js"))
        );
        assert_eq!(resolve(dir.path(), "/../etc/passwd"), None);
        assert_eq!(resolve(dir.path(), "/pkg%2f..%2findex.html"), None);
        assert_eq!(resolve(dir.path(), "/missing"), None);
    }

    #[test]
    fn index_html_subscribes_to_reloads() {
        let body = inject_reload(b"<html><body><p>x</p></body></html>");
        let text = String::from_utf8(body).unwrap();
        assert!(text.contains("new EventSource('/__tilo/events')"));
        assert!(text.ends_with("</script>\n</body></html>"));
    }

    #[test]
    fn a_rebuild_reaches_every_subscriber() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), "<body></body>").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = Arc::new(Server {
            root: dir.path().to_path_buf(),
            clients: Mutex::new(Vec::new()),
        });
        let accepting = Arc::clone(&server);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let _ = handle(&accepting, stream);
            }
        });
        let mut page = TcpStream::connect(address).unwrap();
        page.write_all(b"GET /index.html HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        std::io::Read::read_to_string(&mut page, &mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("EventSource"));

        let events = TcpStream::connect(address).unwrap();
        (&events)
            .write_all(b"GET /__tilo/events HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        let mut reader = BufReader::new(events);
        let mut line = String::new();
        while !line.starts_with(": connected") {
            line.clear();
            reader.read_line(&mut line).unwrap();
        }
        for _ in 0..50 {
            if !server.clients.lock().unwrap().is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        server.broadcast("reload", "{}");
        let mut received = String::new();
        while !received.contains("data: {}") {
            line.clear();
            reader.read_line(&mut line).unwrap();
            received.push_str(&line);
        }
        assert!(received.contains("event: reload"));
    }
}
