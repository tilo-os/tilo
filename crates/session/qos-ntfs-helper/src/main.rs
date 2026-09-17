#[cfg(unix)]
fn main() -> Result<(), std::io::Error> {
    qos_ntfs_helper::broker::command(&std::env::args().skip(1).collect::<Vec<_>>())
}
#[cfg(not(unix))]
fn main() -> Result<(), &'static str> {
    Err("NTFS production inspection requires Linux; no capability on this target")
}
