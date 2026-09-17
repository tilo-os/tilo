//! Native stdio MCP host for one persistent shared-runtime profile.

use shell::runtime_host::mcp_process::{parse_args, run};

fn main() -> std::process::ExitCode {
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .try_init();
    let result =
        parse_args(std::env::args_os().skip(1)).and_then(|args| run(&args.profile, &args.apps));
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qos-runtime-mcp: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
