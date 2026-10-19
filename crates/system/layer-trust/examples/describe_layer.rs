//! How long sealing and describing a layer takes, as tilo-vmd does it before a
//! signature (`tilo_layer_trust::describe`): `describe_layer <layer dir>`.
//! The time is what a swap into the `vm` or `device` slot adds for a new layer.

fn main() -> std::process::ExitCode {
    let Some(root) = std::env::args_os().nth(1) else {
        eprintln!("usage: describe_layer <layer dir>");
        return std::process::ExitCode::from(2);
    };
    let started = std::time::Instant::now();
    match tilo_layer_trust::describe(std::path::Path::new(&root)) {
        Ok(entries) => {
            let sealed = entries
                .iter()
                .filter(|entry| {
                    matches!(
                        entry,
                        tilo_layer_trust::Entry::File {
                            verity: Some(_),
                            ..
                        }
                    )
                })
                .count();
            println!(
                "{} entries ({sealed} files sealed) described in {:.3} s; content {}",
                entries.len(),
                started.elapsed().as_secs_f64(),
                tilo_layer_trust::manifest::content_sha256(&entries)
            );
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("describe_layer: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
