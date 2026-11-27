/// Compile `sources` (each a package's source and its `app.ids`), each against those before it
/// and the platform's packages `platform`: what the build compiles the built-in packages with,
/// and a test its own.
///
/// # Errors
/// A source does not compile: its diagnostics.
fn compile_each(
    sources: &[(&str, &str)],
    platform: &[tilolang_tilo::tilolang::PackageMeta],
) -> Result<Vec<tilolang_tilo::tilolang::Compiled>, String> {
    let mut compiled: Vec<tilolang_tilo::tilolang::Compiled> = Vec::new();
    for (source, ids) in sources {
        let lock =
            tilolang_tilo::tilolang::IdsLock::parse(ids).map_err(|error| error.to_string())?;
        let imports: Vec<_> = compiled
            .iter()
            .map(|done| done.meta.clone())
            .chain(platform.iter().cloned())
            .collect();
        let package = tilolang_tilo::tilolang::compile_package(source, &lock, &imports).map_err(
            |diagnostics| {
                diagnostics
                    .iter()
                    .map(|diagnostic| diagnostic.render("app.tl"))
                    .collect::<Vec<_>>()
                    .join("\n")
            },
        )?;
        compiled.push(package);
    }
    Ok(compiled)
}
