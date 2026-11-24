//! Every app's model, linked into one WebAssembly module.
//!
//! Each app's guest crate (`apps/<app>-model-wasm`) declares its entry with
//! `tilo_model_guest::model!`, which, built here with `--cfg
//! tilo_model_split`, exports `tilo_model_apply_<app>` and names it in the
//! `tilo_split_exports` section. `wasm-split --models` (tools/web/wasm-split)
//! then cuts this module into the model runtime, everything two apps or more
//! reach (automerge, std, the resource layer, `serde_json`), which the shared
//! runtime ships once, and one module per app holding only what its own
//! entry reaches: the `model.wasm` its package carries.
//!
//! Each guest is named below so that it is linked: a dependency nothing
//! names is not.

extern crate activity_model_wasm;
extern crate assistant_model_wasm;
extern crate backups_model_wasm;
extern crate camera_model_wasm;
extern crate console_model_wasm;
extern crate db_client_model_wasm;
extern crate diffmerge_model_wasm;
extern crate disk_usage_model_wasm;
extern crate firewall_model_wasm;
extern crate git_client_model_wasm;
extern crate hex_editor_model_wasm;
extern crate logs_model_wasm;
extern crate maps_model_wasm;
extern crate poker_model_wasm;
extern crate publisher_model_wasm;
extern crate remote_model_wasm;
extern crate scratchpad_model_wasm;
