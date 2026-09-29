{ lib
, rustPlatform
, pkg-config
, cmake
, clang
, capnproto
, buildPackages
, makeWrapper
, bubblewrap
, xdg-dbus-proxy
, libxkbcommon
, wayland
, vulkan-loader
, libglvnd
}:

# The standalone runtime: how Tilo apps run on a NixOS desktop that is not the
# Tilo session. Same payload and layout as the FHS packages (dist/runtime).
rustPlatform.buildRustPackage rec {
  pname = "tilo-runtime";
  version = lib.removeSuffix "\n" (builtins.readFile ../VERSION);
  src = lib.cleanSourceWith {
    src = ../..;
    filter = path: type:
      lib.cleanSourceFilter path type
      && !(type == "directory" && builtins.elem (baseNameOf path) [ "target" "result" ]);
  };

  cargoLock.lockFile = ../../Cargo.lock;
  cargoBuildFlags = [
    "-p" "tilo-runtime"
    "-p" "launcher"
    "-p" "store"
    "--bin" "tilo-runtime"
    "--bin" "tilo-launcher"
    "--bin" "store"
  ];
  doCheck = false;

  CARGO_PROFILE_RELEASE_LTO = "thin";
  CARGO_PROFILE_RELEASE_CODEGEN_UNITS = "16";

  nativeBuildInputs = [ pkg-config cmake clang capnproto makeWrapper ];
  buildInputs = [ libxkbcommon wayland ];
  LIBCLANG_PATH = "${lib.getLib buildPackages.libclang}/lib";

  installPhase = ''
    runHook preInstall
    bin_dir=$(dirname "$(find target -type f -path '*/release/tilo-runtime' -perm -0100 -print -quit)")
    "$src/dist/runtime/stage-payload.sh" "$bin_dir" "$TMPDIR/runtime-payload"
    p="$TMPDIR/runtime-payload"
    private="$out/libexec/tilo-runtime"
    install -Dm755 -t "$private" "$p/libexec/tilo-runtime" "$p/libexec/tilo-launcher" "$p/libexec/store"
    mkdir -p "$out/bin"
    ln -s ../libexec/tilo-runtime/tilo-runtime "$out/bin/tilo-runtime"
    install -Dm644 -t "$out/share/systemd/user" "$p/systemd-user/tilo-runtime.service"
    install -Dm644 -t "$out/share/applications" "$p/applications/so.tilo.store.desktop"
    install -Dm644 -t "$out/share/icons/hicolor/scalable/apps" "$p/icons/so.tilo.store.svg"
    install -Dm644 "$p/license/LICENSE" "$out/share/licenses/tilo-runtime/LICENSE"
    substituteInPlace "$out/share/systemd/user/tilo-runtime.service" \
      --replace-fail 'ExecStart=/usr/bin/tilo-runtime' "ExecStart=$out/bin/tilo-runtime"
    substituteInPlace "$out/share/applications/so.tilo.store.desktop" \
      --replace-fail '/usr/libexec/tilo-runtime/' "$private/"
    runHook postInstall
  '';

  # No FHS PATH or ldconfig cache: seal the sandbox tools into the launcher and
  # the dlopen()ed window and GPU stack into the app.
  postFixup = ''
    wrapProgram "$out/libexec/tilo-runtime/tilo-launcher" \
      --prefix PATH : ${lib.makeBinPath [ bubblewrap xdg-dbus-proxy ]}
    wrapProgram "$out/libexec/tilo-runtime/store" \
      --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath [ vulkan-loader libglvnd wayland libxkbcommon ]}
  '';

  meta = {
    description = "Tilo runtime for running Tilo apps on this desktop";
    homepage = "https://code.tilo.so/tilo/tilo";
    license = lib.licenses.agpl3Only;
    platforms = [ "x86_64-linux" "aarch64-linux" ];
    mainProgram = "tilo-runtime";
  };
}
