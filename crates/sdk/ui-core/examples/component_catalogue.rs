//! Print the toolkit's component catalogue, the file tools that do not link the toolkit read
//! (`crates/dev/ui-graph/components.json`):
//!
//! `cargo run -p tilo-ui --example component-catalogue > crates/dev/ui-graph/components.json`

fn main() {
    print!("{}", tilo_ui::component::catalogue().to_json());
}
