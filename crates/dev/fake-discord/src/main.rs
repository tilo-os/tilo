//! Run a fake Discord on loopback until interrupted, for poking the
//! connector's worker by hand or from an end-to-end journey.
//!
//! `fake-discord [--rest 127.0.0.1:PORT] [--gateway 127.0.0.1:PORT]`
//! prints the environment a `discord-worker` needs to use it.

use std::net::SocketAddr;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let mut rest: SocketAddr = ([127, 0, 0, 1], 0).into();
    let mut gateway: SocketAddr = ([127, 0, 0, 1], 0).into();
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let value = args.next().unwrap_or_default();
        let parsed: SocketAddr = value.parse().map_err(|_| {
            std::io::Error::other(format!(
                "{flag} wants an address like 127.0.0.1:8080, not {value:?}"
            ))
        })?;
        if !parsed.ip().is_loopback() {
            return Err(std::io::Error::other("the fake only listens on loopback"));
        }
        match flag.as_str() {
            "--rest" => rest = parsed,
            "--gateway" => gateway = parsed,
            other => {
                return Err(std::io::Error::other(format!(
                    "unknown flag {other}; use --rest or --gateway"
                )));
            }
        }
    }
    let fake = fake_discord::FakeDiscord::start_on(rest, gateway).await?;
    println!("DISCORD_API_BASE={}", fake.api_base());
    println!("DISCORD_CDN_BASE={}", fake.cdn_base());
    println!("DISCORD_ALLOW_LOOPBACK=1");
    println!(
        "# bot token (store it as the connector's bot-token secret): {}",
        fake_discord::TOKEN
    );
    println!("# gateway: {}", fake.gateway_url());
    tokio::signal::ctrl_c().await?;
    Ok(())
}
