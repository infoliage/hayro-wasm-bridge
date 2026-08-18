# hayro-wasm-bridge

A minimal wasm build of [`hayro`](https://crates.io/crates/hayro) (a pure-Rust
PDF rasterizer) exposing a plain C-style function interface, meant to be
called from a non-browser wasm host — e.g. Go, via a runtime like
[`wazero`](https://wazero.io).

## Why not just use `hayro`'s own demo?

`hayro`'s repo ships `hayro-demo`, which also compiles `hayro` to wasm — but
via [`wasm-bindgen`](https://rustwasm.github.io/wasm-bindgen/), which targets
*browser JavaScript* specifically. Its output `.wasm` file requires a JS
engine behind it (it imports `console.log`, DOM APIs, etc.), so it can't be
loaded by a Go wasm runtime, which has no JS engine to offer.

This crate builds the same underlying `hayro` library with a `cdylib`
target and a hand-written `extern "C"` boundary instead, so the resulting
`.wasm` module has **zero required host imports** — any wasm runtime can
load it.

## Building

```sh
cargo build --target wasm32-unknown-unknown --release
```

produces `target/wasm32-unknown-unknown/release/hayro_wasm_bridge.wasm`.

## Calling convention

wasm functions can only take/return plain numbers — there's no way to hand a
`Vec<u8>` or a struct across the boundary directly. So every exported
function here speaks in terms of `(pointer, length)` pairs into the module's
own linear memory, which the host (Go) reads/writes via whatever byte-buffer
API its wasm runtime exposes (e.g. `mod.Memory().Read`/`.Write` in wazero).

Exported functions:

- `wasm_alloc(size: usize) -> ptr`
  Reserve `size` bytes inside the module and return a pointer to them. Call
  this first, then write your input bytes (e.g. a PDF file) into the
  module's memory starting at the returned pointer.

- `wasm_free(ptr, size)`
  Free a buffer previously returned by `wasm_alloc` or `render_page`. `size`
  must be the same size that was used to allocate it.

- `page_count(pdf_ptr, pdf_len) -> i32`
  Parse the PDF at `pdf_ptr`/`pdf_len` and return its page count, or `-1` if
  it couldn't be parsed.

- `render_page(pdf_ptr, pdf_len, page_number, scale) -> ptr`
  Render one page (`page_number` is **1-based**) at the given `scale`
  (`1.0` = the PDF's native size). Returns a pointer to a buffer laid out
  as:
  - bytes `[0..4)`: pixel width, `u32` little-endian
  - bytes `[4..8)`: pixel height, `u32` little-endian
  - bytes `[8..)`: `width * height * 4` bytes of RGBA8 pixel data

  Returns a null pointer (`0`) on failure. The caller must free a non-null
  result with `wasm_free`, passing `8 + width * height * 4` as the size.

### Typical call sequence from the host

1. `ptr := wasm_alloc(len(pdfBytes))`, write `pdfBytes` into memory at `ptr`.
2. `resultPtr := render_page(ptr, len(pdfBytes), pageNumber, scale)`.
3. `wasm_free(ptr, len(pdfBytes))` — done with the input buffer.
4. If `resultPtr == 0`, rendering failed. Otherwise read 8 header bytes to
   get `width`/`height`, then `width * height * 4` bytes of RGBA8 pixels.
5. `wasm_free(resultPtr, 8 + width*height*4)` once you've copied the pixels
   out to Go-owned memory.

## Status

This is a scaffold, not a finished library. In particular it doesn't yet
handle: panics crossing the wasm boundary (a bug here currently traps the
whole wasm instance rather than returning an error), password-protected
PDFs, or concurrent calls (there's no interior locking — call sequentially,
or instantiate one module per goroutine).
