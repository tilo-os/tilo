//! Native stdio MCP host for one persistent shared-runtime profile.

use shell::runtime_host::mcp_process::{parse_args, prepare_profile_environment, run};

fn main() -> std::process::ExitCode {
    let result = parse_args(std::env::args_os().skip(1)).and_then(|args| {
        let environment = prepare_profile_environment(&args.profile)?;
        for (name, path) in environment {
            // SAFETY: main has not created a runtime, broker, reader thread or
            // child process. This is the executable's single-threaded startup
            // boundary; no reusable serving function mutates the environment.
            unsafe { std::env::set_var(name, path) };
        }
        let _ = tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .try_init();
        run(&args.profile, &args.apps)
    });
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qos-runtime-mcp: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
