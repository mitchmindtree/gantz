# The gantz website (the default site that ships): the app built for cpal's AudioWorklet
# backend, which runs audio on a dedicated Web Audio thread via WASM threads
# (SharedArrayBuffer). It needs a *nightly* toolchain (`-Z build-std` to recompile `std` with
# atomics) and the shared-memory build flags from `wasm-threads-env.nix`. The legacy
# ScriptProcessor build is kept as `gantz-website-legacy` (stable toolchain, no cross-origin
# isolation needed) as a fallback.
#
# This is a plain `mkDerivation` rather than `buildRustPackage` because `-Z build-std`
# recompiles `std` from the rust-src component, so `std`'s own crates.io deps must be vendored
# alongside the app's - the sandbox has no network and `buildRustPackage` only vendors the
# workspace lock.
{
  binaryen,
  lib,
  lld,
  runCommand,
  rustPlatform,
  rustToolchainWasmNightly,
  stdenv,
  trunk,
  wasm-bindgen-cli,
}:
let
  # Everything except build artifacts.
  src = lib.cleanSourceWith {
    src = ../.;
    filter =
      path: type:
      let
        base = baseNameOf (toString path);
      in
      !(builtins.elem base [
        "target"
        "result"
        "dist"
        ".direnv"
      ])
      && lib.cleanSourceFilter path type;
  };

  # Vendor both the workspace deps and `std`'s deps (from the rust-src component the nightly
  # toolchain ships), then merge them into one source-replacement tree so build-std resolves
  # everything offline. Neither lock has git deps, so no `outputHashes` are needed.
  appDeps = rustPlatform.importCargoLock { lockFile = ../Cargo.lock; };
  stdDeps = rustPlatform.importCargoLock {
    lockFile = "${rustToolchainWasmNightly}/lib/rustlib/src/rust/library/Cargo.lock";
  };
  cargoVendor = runCommand "gantz-worklet-vendor" { } ''
    mkdir -p $out
    cp -r ${appDeps}/. $out/
    # Shared crate+version dirs are byte-identical, so skipping collisions is safe.
    cp -rn ${stdDeps}/. $out/
  '';
in
stdenv.mkDerivation (
  {
    pname = "gantz-website";
    version = "0.1.0";
    inherit src;

    nativeBuildInputs = [
      rustToolchainWasmNightly
      binaryen
      lld
      trunk
      wasm-bindgen-cli
    ];

    # Tell trunk to use Nix-provided tools, not download its own; resolve everything from the vendor.
    TRUNK_SKIP_VERSION_CHECK = "true";
    CARGO_NET_OFFLINE = "true";

    configurePhase = ''
      runHook preConfigure
      # trunk (via wasm-bindgen) and cargo need writable home/cache dirs in the sandbox.
      export HOME=$(mktemp -d)
      export CARGO_HOME=$HOME/.cargo
      mkdir -p $CARGO_HOME
      cat > $CARGO_HOME/config.toml <<EOF
      [source.crates-io]
      replace-with = "vendored-sources"
      [source.vendored-sources]
      directory = "${cargoVendor}"
      EOF
      runHook postConfigure
    '';

    buildPhase = ''
      runHook preBuild
      # The worklet page (crates/gantz/web/worklet/index.html) opts into cpal's `audioworklet`
      # feature; the shared build flags (atomics + build-std) come from the derivation env below.
      trunk build --config Trunk.worklet.toml --release --dist $out
      runHook postBuild
    '';

    dontInstall = true;
    dontFixup = true;
  }
  // (import ./wasm-threads-env.nix)
)
