# The legacy website build: cpal's default web backend (the deprecated main-thread
# ScriptProcessor host), on the stable toolchain with no cross-origin-isolation
# requirement. The default `gantz-website` is the AudioWorklet build; this is kept as a
# fallback for hosts/browsers where WASM threads are unavailable.
{
  binaryen,
  lib,
  lld,
  rustPlatform,
  trunk,
  wasm-bindgen-cli,
}:
let
  src = lib.sourceFilesBySuffices ../. [
    ".gantz"
    ".lock"
    ".rs"
    ".toml"
    ".html"
    ".css"
    ".js"
    ".json"
    ".png"
    ".svg"
    ".ico"
  ];
in
rustPlatform.buildRustPackage {
  pname = "gantz-website-legacy";
  version = "0.1.0";
  inherit src;
  cargoLock.lockFile = ../Cargo.lock;
  doCheck = false;
  dontFixup = true;

  nativeBuildInputs = [
    binaryen
    lld
    trunk
    wasm-bindgen-cli
  ];

  # Tell trunk to use Nix-provided tools, not download its own.
  TRUNK_SKIP_VERSION_CHECK = "true";

  # buildRustPackage's configurePhase sets up cargo vendoring.
  # Override buildPhase to call trunk instead of cargo directly.
  buildPhase = ''
    trunk build --release --dist $out
  '';

  installPhase = "true";
}
