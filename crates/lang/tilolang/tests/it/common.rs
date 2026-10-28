use tilolang::{Compiled, Diagnostic, IdsLock, PackageMeta, SeededEntropy};

pub fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

pub fn compile_with(src: &str, imports: &[PackageMeta]) -> Result<Compiled, Vec<Diagnostic>> {
    tilolang::compile_package_with(src, &IdsLock::new(), imports, &mut SeededEntropy(7))
}

pub fn compile(src: &str) -> Compiled {
    compile_with(src, &[]).unwrap_or_else(|d| panic!("{d:#?}"))
}

pub fn compile_fixture(name: &str, imports: &[PackageMeta]) -> Compiled {
    compile_with(&fixture(name), imports).unwrap_or_else(|d| panic!("{name}: {d:#?}"))
}

pub fn todo_meta() -> PackageMeta {
    compile_fixture("todo.tl", &[]).meta
}

/// Error codes reported for a module body (the `app` header is added).
pub fn codes_with(body: &str, imports: &[PackageMeta]) -> Vec<String> {
    let src = format!("app t.x \"X\"\n{body}\n");
    match compile_with(&src, imports) {
        Ok(_) => Vec::new(),
        Err(d) => d.into_iter().map(|d| d.code).collect(),
    }
}

pub fn codes(body: &str) -> Vec<String> {
    codes_with(body, &[])
}

pub fn validate(wasm: &[u8]) {
    wasmparser::Validator::new()
        .validate_all(wasm)
        .unwrap_or_else(|e| panic!("invalid wasm: {e}"));
}

/// Compiles a module body under a fixed `app` header.
pub fn compile_body(body: &str) -> Compiled {
    compile(&format!("app t.x \"X\"\n{body}\n"))
}
