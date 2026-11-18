//! The fake `WebDAV` server ([`cloud_source_dav::fake::FakeDav`]) on its own, holding a copy of a
//! folder, for trying a `WebDAV` client (`tilo migrate connect webdav`) end to end without a real
//! server:
//!
//! ```text
//! cargo build -p cloud-source-dav --features fake-server --example serve_fake
//! target/debug/examples/serve_fake <folder>
//! ```
//!
//! It prints the address to connect to, the user name and the password, then serves until it
//! is stopped. Everything it holds is in memory.

use std::path::Path;

fn main() {
    let Some(folder) = std::env::args_os().nth(1) else {
        eprintln!("usage: serve_fake <folder whose files it serves>");
        std::process::exit(2);
    };
    let server = cloud_source_dav::fake::FakeDav::generic();
    let mut files = 0_usize;
    let mut walking = vec![Path::new(&folder).to_path_buf()];
    while let Some(dir) = walking.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(relative) = path.strip_prefix(&folder) else {
                continue;
            };
            let name = relative.to_string_lossy().replace('\\', "/");
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                server.mkdir(&name);
                walking.push(path);
            } else if let Ok(bytes) = std::fs::read(&path) {
                server.put(&name, &bytes);
                files = files.saturating_add(1);
            }
        }
    }
    println!("url {}", server.root_url());
    println!("login {}", cloud_source_dav::fake::USER);
    println!("password {}", cloud_source_dav::fake::PASSWORD);
    println!("files {files}");
    loop {
        std::thread::park();
    }
}
