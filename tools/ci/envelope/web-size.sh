# The release desktop bundle within its size budget, inside the native envelope
# tools/ci/native-envelope.sh sets up, and its exact private image worker staged
# in envelope/release-compute with the SHA-256 of every file.
rustup target add wasm32-unknown-unknown
if [ "$(wasm-bindgen --version 2>/dev/null)" != "wasm-bindgen $WASM_BINDGEN_VERSION" ]; then
    cargo install --locked wasm-bindgen-cli --version "$WASM_BINDGEN_VERSION"
fi
bash tools/ci/measure.sh "SDK release web tool build" -- cargo build --locked -p tilo-app-cli --bin tilo-app
bash tools/ci/measure.sh "Web module splitter tests" -- cargo test --locked --manifest-path tools/web/wasm-split/Cargo.toml
bundle=$CARGO_TARGET_DIR/tilo-app/web/calculator/bundle
bash tools/ci/measure.sh "SDK release desktop bundle build" -- \
    "$CARGO_TARGET_DIR/debug/tilo-app" build --target web --bundle desktop --release apps/calculator
python3 tools/web/wasm_size_ratchet.py measure desktop "$bundle"
rm -rf envelope/release-compute
python3 tools/ci/release_compute.py stage "$bundle" envelope/release-compute
bash tools/ci/sccache.sh report || true
