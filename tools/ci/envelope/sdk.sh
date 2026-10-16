# The SDK's Linux path, as t's sdk-linux check runs it, inside the native
# envelope tools/ci/native-envelope.sh sets up.
bin=$CARGO_TARGET_DIR/debug
bash tools/ci/measure.sh "tilo-caps tests" -- cargo test --locked -p tilo-caps
bash tools/ci/measure.sh "tilo-app target tests" -- cargo test --locked -p tilo-app-cli --lib targets::
bash tools/ci/measure.sh "tilo-app build" -- \
    cargo build --locked -p tilo-app-cli -p tilo-app-package-cli --bin tilo-app --bin tilo-app-package
bash tools/ci/measure.sh "Calculator for Linux" -- "$bin/tilo-app" build --target linux apps/calculator
rustup target add wasm32-unknown-unknown
bash tools/ci/measure.sh "generated app outside the repository" -- \
    cargo test --locked -p tilo-app-cli --test it outside_repo -- --ignored --nocapture
TILO_PKG=$bin/tilo-app-package CARGO_PROFILE_THIN_APP_OPT_LEVEL=1 CARGO_PROFILE_THIN_APP_CODEGEN_UNITS=16 \
CARGO_PROFILE_THIN_APP_DEBUG=0 bash tools/ci/measure.sh "Calculator .tapp" -- \
    "$bin/tilo-app" package --target linux apps/calculator --output "${TMPDIR:-/tmp}/calculator.tapp" --force
test -s "${TMPDIR:-/tmp}/calculator.tapp"
bash tools/ci/sccache.sh report || true
