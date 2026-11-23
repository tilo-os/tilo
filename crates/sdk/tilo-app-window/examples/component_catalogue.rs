//! Print the catalogue of every component a window builds, the file tools that do not link the
//! toolkit read (`crates/ui/ui-graph/components.json`):
//!
//! `cargo run -p tilo-app-window --example component-catalogue > crates/ui/ui-graph/components.json`

fn main() {
    print!("{}", tilo_app_window::adapter::catalogue().to_json());
}
