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

Memory is allocated and freed with a dedicated pair of functions per kind of
thing crossing the boundary, rather than one generic `alloc(size)`/
`free(ptr, size)` pair. For the fixed-size kinds this means the host never
has to know or pass back a size — the function you call *is* the size, so a
mismatch between what a buffer was allocated with and what it's freed with
isn't expressible, and it can't drift out of sync with a newer/older build
of this module the way a host-hardcoded size constant could.

Exported functions:

- `alloc_pdf(size: usize) -> ptr` / `free_pdf(ptr, size)`
  Reserve `size` bytes for a PDF file's bytes, and free a buffer previously
  returned by `alloc_pdf`. Unlike the pairs below, this one is genuinely
  variable-length, so `size` must still be passed back unchanged to
  `free_pdf`.

- `alloc_render_settings() -> ptr` / `free_render_settings(ptr)`
  Reserve a 16-byte, zeroed render-settings blob (see `render_page` below
  for the layout), and free one.

- `alloc_interpreter_settings() -> ptr` / `free_interpreter_settings(ptr)`
  Reserve a 1-byte, zeroed interpreter-settings blob (see `render_page`
  below for the layout), and free one.

- `alloc_u32() -> ptr` / `free_u32(ptr)`
  Reserve a zeroed 4-byte `u32` cell, for use as one of `render_page`'s
  `width_out`/`height_out` arguments, and free one.

- `page_count(pdf_ptr, pdf_len) -> i32`
  Parse the PDF at `pdf_ptr`/`pdf_len` and return its page count, or `-1` if
  it couldn't be parsed.

- `render_page(pdf_ptr, pdf_len, page_number, interpreter_settings_ptr, render_settings_ptr, width_out, height_out) -> ptr`
  Render one page (`page_number` is **1-based**) to RGBA8 pixels
  (non-premultiplied, one byte per channel).

  `interpreter_settings_ptr`/`render_settings_ptr` each select a `hayro`
  settings struct for this render — pass `0` (null) for either to use
  `hayro`'s defaults, or a pointer to a fixed-length blob obtained from
  `alloc_render_settings`/`alloc_interpreter_settings`:

  - **Render settings** (16 bytes, mirrors `hayro::RenderSettings`):
    - `[0..4)`: `x_scale`, `f32` little-endian; `0.0` means "use the
      default" (`1.0`)
    - `[4..8)`: `y_scale`, `f32` little-endian; same `0.0` meaning
    - `[8..10)`: `width`, `u16` little-endian; `0` = auto
    - `[10..12)`: `height`, `u16` little-endian; `0` = auto
    - `[12..16)`: `bg_color` as R, G, B, A bytes, **straight
      (non-premultiplied) alpha** — matches Go's `image/color.NRGBA`, *not*
      `color.RGBA` (which is premultiplied). All-zero = fully transparent
      black, `hayro`'s actual default.

    Every field's `0` value was deliberately chosen to mean "use the
    default" — including `x_scale`/`y_scale`, where an explicit `0.0` would
    always produce a zero-area (and thus failing) render anyway, so nothing
    is lost by giving it this meaning instead. That's what makes a
    zero-initialized blob from `alloc_render_settings` behave exactly like
    `RenderSettings::default()`.
  - **Interpreter settings** (1 byte, mirrors one field of
    `hayro_interpret::InterpreterSettings`):
    - `[0..1)`: `render_annotations`; `0` = use `hayro`'s default (read from
      `InterpreterSettings::default()` at call time, not hardcoded, so this
      stays correct even if that default ever changes), `1` = enabled,
      anything else = disabled

    `InterpreterSettings`'s `font_resolver`/`cmap_resolver`/`warning_sink`
    fields are Rust closures and can't be represented as plain bytes, so
    they're left at `hayro`'s defaults — this crate enables `embed-fonts`
    and `embed-cmaps`, so those defaults still resolve real font/cmap data.

  `width_out`/`height_out` must each point to a 4-byte `u32` cell (obtained
  from `alloc_u32`); on success the rendered pixel width/height are written
  there.

  Returns a pointer to `width * height * 4` bytes of pixel data, or a null
  pointer (`0`) on failure (unparseable PDF, out-of-range `page_number`, or
  a zero-area render) — in which case `width_out`/`height_out` are left
  untouched. The caller must free a non-null result with `free_pixels`,
  passing the same `width`/`height` this function wrote to `width_out`/
  `height_out`.

- `free_pixels(ptr, width, height)`
  Free a pixel buffer previously returned by `render_page`, given the exact
  `width`/`height` it reported for that call.

### Typical call sequence from the host

1. `pdfPtr := alloc_pdf(len(pdfBytes))`, write `pdfBytes` into memory at
   `pdfPtr`.
2. Optionally call `alloc_render_settings`/`alloc_interpreter_settings` and
   write into the result, if you want anything other than `hayro`'s
   defaults — you only need to write the fields you care about, since both
   buffers start zeroed and `0` always means "use the default".
3. `widthOutPtr := alloc_u32()`, `heightOutPtr := alloc_u32()`.
4. `resultPtr := render_page(pdfPtr, len(pdfBytes), pageNumber, interpreterSettingsPtr, renderSettingsPtr, widthOutPtr, heightOutPtr)`.
5. `free_pdf(pdfPtr, len(pdfBytes))` and free any settings blobs — done
   with the input buffers.
6. If `resultPtr == 0`, rendering failed. Otherwise read `width`/`height`
   back from `widthOutPtr`/`heightOutPtr`, then `width * height * 4` bytes
   of RGBA8 pixels from `resultPtr`.
7. `free_pixels(resultPtr, width, height)`, and `free_u32` the two output
   cells, once you've copied everything out to Go-owned memory.

## Status

This is a scaffold, not a finished library. In particular it doesn't yet
handle: panics crossing the wasm boundary (a bug here currently traps the
whole wasm instance rather than returning an error), password-protected
PDFs, or concurrent calls (there's no interior locking — call sequentially,
or instantiate one module per goroutine).
