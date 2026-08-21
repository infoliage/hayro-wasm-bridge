# hayro-wasm-bridge

A minimal wasm build of [`hayro`](https://crates.io/crates/hayro) (a pure-Rust
PDF rasterizer) exposing a C-style function interface, meant to be called from
a non-browser wasm host — e.g. Go, via a runtime like
[`wazero`](https://wazero.io).

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

The two Hayro settings blobs (`RenderSettings`/`InterpreterSettings`) are UTF-8
**JSON**. Every field in both objects is optional; an absent field means "use
`hayro`'s default for that field". `schema/*.json` in this repo documents both
shapes.

Exported functions:

- `alloc_pdf(size: usize) -> ptr` / `free_pdf(ptr, size)`
  Reserve `size` bytes for a PDF file's bytes, and free a buffer previously
  returned by `alloc_pdf`.

- `alloc_render_settings(size: usize) -> ptr` / `free_render_settings(ptr, size)`
  Reserve `size` bytes for a render-settings JSON blob (see `render_page`
  below for the shape), and free one.

- `alloc_interpreter_settings(size: usize) -> ptr` / `free_interpreter_settings(ptr, size)`
  Reserve `size` bytes for an interpreter-settings JSON blob (see
  `render_page` below for the shape), and free one.

- `alloc_u32() -> ptr` / `free_u32(ptr)`
  Reserve a zeroed 4-byte `u32` cell, for use as one of `render_page`'s
  `width_out`/`height_out` arguments, and free one.

- `page_count(pdf_ptr, pdf_len) -> i32`
  Parse the PDF at `pdf_ptr`/`pdf_len` and return its page count, or `-1` if
  it couldn't be parsed.

- `render_page(pdf_ptr, pdf_len, page_number, interpreter_settings_ptr, interpreter_settings_len, render_settings_ptr, render_settings_len, width_out, height_out) -> ptr`
  Render one page (`page_number` is **1-based**) to RGBA8 pixels
  (non-premultiplied, one byte per channel).

  `interpreter_settings_ptr`/`render_settings_ptr` each independently select
  a `hayro` settings struct for this render, as UTF-8 JSON text — pass a
  null pointer (with a length of `0`) for either to use all of `hayro`'s
  defaults for that struct, or a pointer/length obtained from
  `alloc_render_settings`/`alloc_interpreter_settings`. Malformed JSON, or an
  object with an unrecognized field name, is a hard failure (see the return
  value below) — not silently treated as "use the defaults".

  - **Render settings** (`schema/render-settings.schema.json`, mirrors
    `hayro::RenderSettings`), all fields optional:
    - `x_scale`, `y_scale`: numbers, leave absent for `hayro`'s default of
      `1.0` for each.
    - `width`, `height`: integers in `0..=65535`. Absent means "auto".
    - `bg_color`: `{"r": .., "g": .., "b": .., "a": ..}`, each `0..=255`,
      **straight (non-premultiplied) alpha** — matches Go's
      `image/color.NRGBA`, *not* `color.RGBA`.  Absent means `hayro`'s
      default (`#00000000`).

    Example: `{"width": 800, "bg_color": {"r": 255, "g": 255, "b": 255, "a": 255}}`
    leaves `x_scale`/`y_scale`/`height` at `hayro`'s defaults.

  - **Interpreter settings** (`schema/interpreter-settings.schema.json`,
    mirrors part of `hayro_interpret::InterpreterSettings`):
    - `render_annotations`: boolean. Absent means "use `hayro`'s default".

    `InterpreterSettings`'s `font_resolver`/`cmap_resolver`/`warning_sink`
    fields are Rust closures and can't be represented as JSON, so they're
    left at `hayro`'s defaults — this crate enables `embed-fonts` and
    `embed-cmaps`, so those defaults still resolve real font/cmap data.

  `width_out`/`height_out` must each point to a 4-byte `u32` cell (obtained
  from `alloc_u32`); on success the rendered pixel width/height are written
  there.

  Returns a pointer to `width * height * 4` bytes of pixel data, or a null
  pointer (`0`) on failure (unparseable PDF, out-of-range `page_number`,
  malformed settings JSON, or a zero-area render) — in which case
  `width_out`/`height_out` are left untouched. The caller must free a
  non-null result with `free_pixels`.

- `free_pixels(ptr, width, height)`
  Free a pixel buffer previously returned by `render_page`, given the exact
  `width`/`height` it reported for that call.

### Typical call sequence from the host

1. `pdfPtr := alloc_pdf(len(pdfBytes))`, write `pdfBytes` into memory at
   `pdfPtr`.
2. Optionally marshal a settings struct to JSON, `ptr := alloc_render_settings(len(json))` /
   `alloc_interpreter_settings(len(json))`, and write it into the result —
   only including the fields you want to be non-default.
3. `widthOutPtr := alloc_u32()`, `heightOutPtr := alloc_u32()`.
4. `resultPtr := render_page(pdfPtr, len(pdfBytes), pageNumber, interpreterSettingsPtr, interpreterSettingsLen, renderSettingsPtr, renderSettingsLen, widthOutPtr, heightOutPtr)`.
5. `free_pdf(pdfPtr, len(pdfBytes))` and free any settings blobs — done
   with the input buffers.
6. If `resultPtr == 0`, rendering failed. Otherwise read `width`/`height`
   back from `widthOutPtr`/`heightOutPtr`, then `width * height * 4` bytes
   of RGBA8 pixels from `resultPtr`.
7. `free_pixels(resultPtr, width, height)`, and `free_u32` the two output
   cells, once you've copied everything out to Go-owned memory.

## Future work

This code doesn't yet handle: panics crossing the wasm boundary (a bug here
currently traps the whole wasm instance rather than returning an error);
password-protected PDFs; concurrent calls (there's no interior locking —
call sequentially, or instantiate one module per goroutine).
