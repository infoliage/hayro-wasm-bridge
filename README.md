# hayro-wasm-bridge

A WASM build of [`hayro`](https://crates.io/crates/hayro) (a pure-Rust PDF
rasterizer) exposing a C-style function interface, meant to be called from a
non-browser wasm host — e.g. Go, via a runtime like
[`wazero`](https://wazero.io).  The resulting `.wasm` is about 5MB, and has
zero required host imports, so any wasm runtime can
load it.  This build enables https://github.com/infoliage/hayro-wasm-go/.

## Building

```sh
cargo build --target wasm32-unknown-unknown --release
```

produces `target/wasm32-unknown-unknown/release/hayro_wasm_bridge.wasm`.

Note that `.cargo/config.toml` enables the `simd128` target feature for this
build, and so the WASM host must have SIMD enabled (wazero and wasmtime both
qualify).

## Docs

```sh
cargo doc --no-deps --open
```

`schema/*.json` holds the same shapes as JSON Schema, for generating a
host-side (e.g. Go) type from.

## Future work

This code doesn't yet handle: panics crossing the wasm boundary (a bug here
currently traps the whole wasm instance rather than returning an error);
password-protected PDFs; concurrent calls (there's no interior locking —
call sequentially, or instantiate one module per goroutine).
