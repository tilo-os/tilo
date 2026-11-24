//! `discord-worker`: started by the connector host, which talks to it over
//! stdin and stdout. Settings come from the environment (see
//! [`discord_worker::config`]); secrets only from the host.

use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    let config = match discord_worker::Config::from_vars(|name| std::env::var(name).ok()) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("discord-worker: {error}");
            return ExitCode::FAILURE;
        }
    };
    tilo_net::install_crypto_provider();
    let input = tokio::io::BufReader::new(tokio::io::stdin());
    match discord_worker::run(config, input, tokio::io::stdout()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("discord-worker: {error}");
            ExitCode::FAILURE
        }
    }
}
