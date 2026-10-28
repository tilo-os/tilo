//! The fetch proxy for development: [`tilo_net::proxy::host`] on loopback,
//! with nobody to authenticate, behind
//! `crates/browser/tilo-web-desktop/scripts/serve.mjs --fetch-proxy`.
//!
//! usage: `tilo-fetch-proxy [--listen 127.0.0.1:0] [--allow-private]`
//!
//! - `--listen` must be a loopback address: the proxy authenticates nobody.
//! - `--allow-private` lets it reach loopback and private addresses (a local
//!   test server); refused by default, as on a workspace host.
//!
//! It serves wire protocol v1 at [`ENDPOINT`] and prints one JSON line,
//! `{"listening": "http://127.0.0.1:<port>"}`, once it accepts connections.
//! Workspace hosts serve the same implementation behind their sessions.

use std::convert::Infallible;
use std::net::SocketAddr;

use tilo_net::proxy::ProxyFailure;
use tilo_net::proxy::host::{Caller, Config, Destinations, FetchProxy};

/// Where the development server's pages find the proxy.
const ENDPOINT: &str = "/_tilo/net/fetch";

fn main() -> std::process::ExitCode {
    let mut listen: SocketAddr = SocketAddr::from(([127, 0, 0, 1], 0));
    let mut destinations = Destinations::Public;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--allow-private" => destinations = Destinations::Any,
            "--listen" => match args.next().and_then(|value| value.parse().ok()) {
                Some(address) => listen = address,
                None => return usage("--listen needs an address such as 127.0.0.1:8788"),
            },
            "--help" | "-h" => return usage(""),
            other => return usage(&format!("unknown argument {other}")),
        }
    }
    if !listen.ip().is_loopback() {
        return usage("--listen must be a loopback address: this proxy authenticates nobody");
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => return fail(&format!("start the runtime: {error}")),
    };
    match runtime.block_on(serve(listen, destinations)) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => fail(&error),
    }
}

fn usage(problem: &str) -> std::process::ExitCode {
    if !problem.is_empty() {
        eprintln!("tilo-fetch-proxy: {problem}");
    }
    eprintln!("usage: tilo-fetch-proxy [--listen 127.0.0.1:0] [--allow-private]");
    std::process::ExitCode::from(2)
}

fn fail(problem: &str) -> std::process::ExitCode {
    eprintln!("tilo-fetch-proxy: {problem}");
    std::process::ExitCode::FAILURE
}

async fn serve(listen: SocketAddr, destinations: Destinations) -> Result<(), String> {
    let proxy = FetchProxy::new(Config {
        destinations,
        ..Config::default()
    })
    .map_err(|error| error.to_string())?;
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(|error| format!("listen on {listen}: {error}"))?;
    let address = listener.local_addr().map_err(|error| error.to_string())?;
    println!("{{\"listening\": \"http://{address}\"}}");
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                let proxy = proxy.clone();
                tokio::spawn(async move {
                    let service = hyper::service::service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                        let proxy = proxy.clone();
                        async move {
                            Ok::<_, Infallible>(if request.uri().path() == ENDPOINT {
                                proxy.serve(Caller::LOCAL, request).await
                            } else {
                                proxy.refuse(ProxyFailure::BadRequest, "the fetch proxy is at /_tilo/net/fetch")
                            })
                        }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                        .await;
                });
            }
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}
