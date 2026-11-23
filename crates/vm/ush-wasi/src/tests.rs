#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::atomic::{AtomicUsize, Ordering};

use ush_shell::{MemoryHost, Output};

use super::*;

fn module(wat: &str) -> Vec<u8> {
    wat::parse_str(wat).unwrap()
}

fn invoke(bytes: &[u8], stop: &dyn Fn() -> bool) -> (Program, String, String) {
    let (stdout, out) = Output::buffer();
    let (stderr, err) = Output::buffer();
    let io = Stdio {
        stdin: ush_shell::Input::null(),
        stdout,
        stderr,
    };
    let argv = ["tool".to_owned(), "an argument".to_owned()];
    let ran = run(
        bytes,
        Arc::new(MemoryHost::new()),
        &Invocation {
            argv: &argv,
            env: &[("GREETING".to_owned(), "hello".to_owned())],
            cwd: Path::new("/"),
            io: &io,
            run: None,
            stop,
        },
    );
    (
        ran,
        String::from_utf8_lossy(&out.take()).into_owned(),
        String::from_utf8_lossy(&err.take()).into_owned(),
    )
}

#[test]
fn a_program_that_runs_on_is_stopped_between_slices() {
    let spin = module(
        r#"(module (memory (export "memory") 1)
        (func (export "_start") (loop $l (br $l))))"#,
    );
    let asked = AtomicUsize::new(0);
    let (ran, _, _) = invoke(&spin, &|| asked.fetch_add(1, Ordering::SeqCst) >= 2);
    assert_eq!(ran, Program::Exited(STOPPED));
    assert_eq!(
        asked.load(Ordering::SeqCst),
        3,
        "it ran two slices, then stopped"
    );
}

#[test]
fn a_program_reads_its_arguments_and_variables_and_ends_with_its_status() {
    // Prints the second argument's length and the variables' bytes count as two digits... kept
    // simple: it writes argument 1 to stdout and exits with the number of variables.
    let echo = module(
        r#"(module
      (import "wasi_snapshot_preview1" "args_sizes_get" (func $sizes (param i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "args_get" (func $args (param i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "environ_sizes_get" (func $esizes (param i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "fd_write" (func $write (param i32 i32 i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "proc_exit" (func $exit (param i32)))
      (memory (export "memory") 1)
      (func (export "_start")
        (drop (call $sizes (i32.const 0) (i32.const 4)))
        (drop (call $args (i32.const 100) (i32.const 200)))
        ;; argument 1 runs from its pointer to the end of the buffer, less its NUL
        (i32.store (i32.const 50) (i32.load (i32.const 104)))
        (i32.store (i32.const 54) (i32.sub (i32.sub (i32.add (i32.const 200) (i32.load (i32.const 4)))
          (i32.load (i32.const 104))) (i32.const 1)))
        (drop (call $write (i32.const 1) (i32.const 50) (i32.const 1) (i32.const 60)))
        (drop (call $esizes (i32.const 8) (i32.const 12)))
        (call $exit (i32.load (i32.const 8)))))"#,
    );
    let (ran, out, err) = invoke(&echo, &|| false);
    assert_eq!(out, "an argument", "{err}");
    assert_eq!(ran, Program::Exited(1));
}

#[test]
fn a_module_that_is_no_wasi_program_says_so() {
    let library = module(
        r#"(module (func (export "add") (param i32 i32) (result i32)
        (i32.add (local.get 0) (local.get 1))))"#,
    );
    let (ran, _, err) = invoke(&library, &|| false);
    assert_eq!(ran, Program::Exited(FAILED));
    assert!(err.contains("no `_start`"), "{err}");
    let (ran, _, err) = invoke(b"not wasm", &|| false);
    assert_eq!(ran, Program::Exited(FAILED));
    assert!(err.starts_with("tool: "), "{err}");
}
