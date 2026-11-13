//! The runtime host a Tilo login runs: the persistent daemon, app attachment
//! and stdio MCP entry point over the profile's own services, beside the
//! desktop session. `runtime_portal::native` looks for this name inside a
//! login, where an app attaches to the session's runtime rather than to one of
//! its own.
use std::process::ExitCode;

use tilo_runtime::standalone::Standalone;

fn main() -> ExitCode {
    // The tools a hosted workspace offers an agent, without a workspace or a
    // session: `tools/docs/reference/mcp_tools.py` writes their reference from
    // this answer, so the page is the server's own list.
    if std::env::args().nth(1).as_deref() == Some("--tools-json") {
        println!("{}", tilo_runtime::mcp::tools_json());
        return ExitCode::SUCCESS;
    }
    tilo_runtime::cli::host::<Standalone>("tilo-runtime-mcp")
}
