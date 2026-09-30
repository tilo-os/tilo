use std::path::{Path, PathBuf};

use ui::launch::{LaunchArgs, LaunchError, LaunchSpec, run_or_exit};
use ui::persist::DocWriter;
use ui::{State, Theme, UiGraphEditor, UiGraphHandle, Widget};

const APP_ID: &str = "so.tilo.ui-builder";

fn main() {
    run_or_exit(
        &LaunchSpec::new_with_args(APP_ID, root)
            .max_positionals(1)
            .open_state_with_args(transient_preview_state),
    );
}

fn transient_preview_state(_: &'static str, _: &LaunchArgs) -> (State, DocWriter) {
    (
        State::new(),
        DocWriter::disabled("UI builder preview state is session-local"),
    )
}

fn root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    let path = args
        .positionals()
        .first()
        .map(PathBuf::from)
        .ok_or_else(|| LaunchError::Operation("tilo-ui-builder requires a UI graph path".into()))?;
    let source =
        std::fs::read_to_string(&path).map_err(|error| operation("read", &path, &error))?;
    let handle = UiGraphHandle::from_json(&source)
        .map_err(|error| LaunchError::Operation(format!("invalid {}: {error}", path.display())))?;
    let save_path = path.clone();
    let editor = UiGraphEditor::new(handle, Theme::dark())
        .map_err(|error| LaunchError::Operation(format!("build {}: {error}", path.display())))?
        .on_save(move |source| save(&save_path, source));
    Ok(Box::new(editor))
}

fn save(path: &Path, source: &str) -> Result<(), String> {
    let mut canonical = source.to_owned();
    if !canonical.ends_with('\n') {
        canonical.push('\n');
    }
    #[cfg(unix)]
    let result = {
        use std::os::unix::fs::PermissionsExt as _;

        let mode = std::fs::metadata(path).map_or(0o644, |metadata| metadata.permissions().mode());
        tilo_files::native::atomic::write_with_mode(path, canonical.as_bytes(), mode)
    };
    #[cfg(not(unix))]
    let result = tilo_files::native::atomic::write(path, canonical.as_bytes());
    result.map_err(|error| format!("write {}: {error}", path.display()))
}

fn operation(action: &str, path: &Path, error: &std::io::Error) -> LaunchError {
    LaunchError::Operation(format!("{action} {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn save_replaces_a_graph_without_leaving_a_partial_file() {
        let directory = std::env::temp_dir().join(format!(
            "tilo-ui-builder-test-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("save")
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("screen.json");
        std::fs::write(&path, "long previous contents").unwrap();
        #[cfg(unix)]
        {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        }
        save(&path, "{}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}\n");
        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}
