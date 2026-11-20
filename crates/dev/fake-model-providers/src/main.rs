//! Serve the fake model providers on loopback until interrupted, for a local stack's manual
//! proof of signing in and switching models.
//!
//! `fake-model-providers [--listen 127.0.0.1:PORT]` prints the four addresses a server is
//! configured with: the sign-in's issuer, the `ChatGPT` backend, `OpenAI`'s API and
//! Anthropic's. Each line is flushed as it is written, so a stack can wait for the first.

use std::io::Write as _;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let mut addr: std::net::SocketAddr = ([127, 0, 0, 1], 0).into();
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let value = args.next().unwrap_or_default();
        match flag.as_str() {
            "--listen" | "--addr" => {
                addr = value.parse().map_err(|_| {
                    std::io::Error::other(format!(
                        "{flag} wants an address like 127.0.0.1:8080, not {value:?}"
                    ))
                })?;
            }
            other => {
                return Err(std::io::Error::other(format!(
                    "unknown flag {other}; use --listen 127.0.0.1:PORT"
                )));
            }
        }
    }
    let fake = fake_model_providers::FakeProviders::start_on(addr).await?;
    let mut out = std::io::stdout();
    for line in [
        format!("OPENAI_ISSUER={}", fake.issuer_url()),
        format!("CHATGPT_BACKEND={}", fake.chatgpt_backend_url()),
        format!("OPENAI_API={}", fake.openai_api_url()),
        format!("ANTHROPIC_API={}", fake.anthropic_api_url()),
    ] {
        writeln!(out, "{line}")?;
    }
    out.flush()?;
    tokio::signal::ctrl_c().await?;
    Ok(())
}
