use std::path::PathBuf;

pub fn state_json_path() -> PathBuf {
    dirs_next::config_dir()
        .map(|c| c.join("quick-desktop").join("state.json"))
        .unwrap_or_else(|| PathBuf::from("state.json"))
}
