//! A process that confined itself to a policy refusing `exec` cannot start
//! the helper: [`Engine::new`] must choose the portable engine there rather
//! than an engine that fails on its first document.
//!
//! An app worker (`tilo-agent-participant`) confines itself exactly this way
//! and then runs the PDF viewer's own code, so this is the choice that makes
//! an agent's PDFs read at all.

#![cfg(unix)]

use tilo_pdf::Engine;

/// Declared in this file's own test binary, so the process-wide declaration
/// reaches no other test.
#[test]
fn a_process_that_runs_no_program_gets_the_portable_engine() {
    tilo_sandbox::confine::declare_no_exec();
    assert!(
        tilo_sandbox::confine::no_exec(),
        "the declaration is readable"
    );
    let engine = Engine::new().expect("an engine without a helper");
    assert!(
        !engine.is_sandboxed(),
        "an engine in a process refusing exec cannot be the helper"
    );
    // The portable engine reads the documents the helper would: a PDF the
    // viewer's own tests open.
    let pdf = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\
                2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n\
                3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] >>\nendobj\n\
                trailer\n<< /Root 1 0 R >>\n%%EOF\n";
    let doc = engine
        .open_bytes(pdf)
        .expect("the portable engine opens a PDF");
    assert_eq!(doc.page_count(), 1);
}
