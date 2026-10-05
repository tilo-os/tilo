//! Code — thin binary entry point. All logic lives in the library.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

/// What the `terminal` scenario types into the live shell: a test run, as a
/// person working in this project would have it on screen.
const TERMINAL_SESSION: &str = "printf 'running 1 test\\ntest prime::tests::primes_below_twenty ... ok\\n\\ntest result: ok. 1 passed; 0 failed\\n'\n";

#[cfg(feature = "thin")]
extern crate runtime_dylib;

ui::main!(code_editor::CodeEditorShell);
