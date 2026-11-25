//! The shell package (`apps/tl/shell`) compiles over the session's actions as
//! `apps/tl/session/surface.json` declares them, the same compile a session starting it runs.
//! A shell calling an action the session no longer declares fails here, by its diagnostic,
//! instead of as every session test and walk dying at startup.

#[test]
fn the_shell_compiles_over_the_session_s_declared_actions() {
    tilolang_tilo::shell::compile()
        .unwrap_or_else(|why| panic!("the shell does not compile:\n{why}"));
}
