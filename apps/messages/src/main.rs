#[cfg(feature = "thin")]
extern crate runtime_dylib;
#[cfg(feature = "runtime-engine")]
extern crate tilo_contact_directory_engine;

ui::main!(messages::MessagesShell);
