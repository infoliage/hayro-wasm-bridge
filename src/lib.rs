//! A minimal C-ABI wasm bridge around the `hayro` PDF rasterizer.
//!
//! This is deliberately *not* built on `wasm-bindgen` (see the README for
//! why). Instead every function here speaks only in plain numbers — the one
//! thing every wasm host, including a Go one, understands natively:
//! pointers (just an offset into the module's own memory, given as a
//! `usize`/`u32`) and lengths. There is no way to hand a Rust `Vec<u8>` or
//! `struct` across the boundary directly, so the host and this module
//! cooperate by reading and writing bytes at agreed-upon offsets in the
//! module's linear memory instead.
//!
//! Memory is allocated and freed with a dedicated pair of functions per
//! kind of thing crossing the boundary ([`alloc_pdf`]/[`free_pdf`],
//! [`alloc_render_settings`]/[`free_render_settings`], and so on), rather
//! than one generic `alloc(size)`/`free(ptr, size)` pair. The one thing
//! this buys the fixed-size kinds — the function you call *is* the size,
//! so a mismatch between what a buffer was allocated with and what it's
//! freed with isn't expressible — doesn't apply to any of these anymore:
//! the settings blobs are JSON now, so every buffer here is genuinely
//! variable-length and the host must pass the size back to free it, same
//! as [`alloc_pdf`]/[`free_pdf`] always required.
//!
//! The settings blobs are UTF-8 JSON text, one object each, matching
//! [`RenderSettings`]/`InterpreterSettings`'s field names. Every field is
//! optional; an absent field means "use `hayro`'s default for it" — see
//! [`render_page`] for the exact shape of each, and `schema/*.json` in this
//! crate's repo for a JSON Schema description of both, kept as
//! documentation (not validated against at runtime — `serde`'s own typed
//! deserialization already rejects anything the schema would catch: wrong
//! types, out-of-range numbers, unknown fields).
//!
//! Calling convention, from the host's side:
//! 1. Call [`alloc_pdf`] to reserve space for the PDF's bytes, and write
//!    them into the module's memory at the returned offset.
//! 2. Optionally call [`alloc_render_settings`] and/or
//!    [`alloc_interpreter_settings`] and write a JSON object into the
//!    result, if you want anything other than `hayro`'s defaults — see
//!    [`render_page`] for the shape of each. Omit whichever fields you
//!    don't care about; pass a null pointer (and a length of `0`) for
//!    either blob entirely to use every one of `hayro`'s defaults.
//! 3. Call [`alloc_u32`] twice, for [`render_page`]'s `width_out`/
//!    `height_out` arguments.
//! 4. Call [`render_page`]. It returns a pointer to `width * height * 4`
//!    bytes of RGBA8 pixel data (non-premultiplied, one byte per channel),
//!    or `0` = null on failure (which now also includes malformed settings
//!    JSON, not just an unparseable PDF, an out-of-range page, or a
//!    zero-area render).
//! 5. Free everything you allocated: [`free_pdf`], [`free_render_settings`]/
//!    [`free_interpreter_settings`] if you used them, [`free_u32`] (for
//!    each of the two output cells), and [`free_pixels`] for the result.

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::AlphaColor;
use hayro::{RenderCache, RenderSettings};
use serde::Deserialize;
use std::alloc::{Layout, alloc, dealloc};

#[cfg(test)]
mod tests;

/// The JSON shape of a render-settings blob — see [`render_page`]'s docs,
/// or `schema/render-settings.schema.json`, for the authoritative
/// description of each field.
///
/// Every field is a real `Option<T>`: absent means "use `hayro`'s
/// default", the same as it always meant. Unlike the old fixed-byte-layout
/// version of this crate, there's no sentinel value doing double duty
/// anymore — an explicit `"x_scale": 0.0`, for example, is now honored
/// literally (and, as before, produces a zero-area — and thus failing —
/// render), rather than being silently reinterpreted as "use the default
/// instead".
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RenderSettingsJson {
    x_scale: Option<f32>,
    y_scale: Option<f32>,
    width: Option<u16>,
    height: Option<u16>,
    bg_color: Option<RgbaJson>,
}

/// Straight (non-premultiplied) alpha, R/G/B/A, matching Go's
/// `image/color.NRGBA` (not `color.RGBA`, which is premultiplied).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RgbaJson {
    r: u8,
    g: u8,
    b: u8,
    a: u8,
}

/// The JSON shape of an interpreter-settings blob — see [`render_page`]'s
/// docs, or `schema/interpreter-settings.schema.json`, for the
/// authoritative description.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InterpreterSettingsJson {
    render_annotations: Option<bool>,
}

/// Allocate `size` bytes inside this module's own memory for the host to
/// write a PDF file's bytes into, and return a pointer to the start of it.
///
/// Every pointer this function hands back must eventually be passed to
/// [`free_pdf`], with the exact same `size` used to allocate it — Rust's
/// allocator needs that size again to free the memory correctly; there's no
/// separate bookkeeping of "how big was this block" the way libc's
/// `malloc`/`free` do it for you.
#[unsafe(no_mangle)]
pub extern "C" fn alloc_pdf(size: usize) -> *mut u8 {
    alloc_bytes(size)
}

/// Free a buffer previously returned by [`alloc_pdf`]. `size` must be the
/// exact size that was originally allocated.
///
/// # Safety
/// `ptr` must be null (in which case this is a no-op) or a pointer
/// previously returned by [`alloc_pdf`] that hasn't already been freed,
/// paired with the exact `size` it was allocated with.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_pdf(ptr: *mut u8, size: usize) {
    // SAFETY: the caller upholds this function's safety contract.
    unsafe { free_bytes(ptr, size) };
}

/// Allocate `size` bytes inside this module's own memory for the host to
/// write a render-settings JSON blob into (see [`render_page`] for the
/// shape), and return a pointer to it. `size` must be the exact byte
/// length of the JSON text that will be written.
#[unsafe(no_mangle)]
pub extern "C" fn alloc_render_settings(size: usize) -> *mut u8 {
    alloc_bytes(size)
}

/// Free a buffer previously returned by [`alloc_render_settings`]. `size`
/// must be the exact size that was originally allocated.
///
/// # Safety
/// `ptr` must be null (in which case this is a no-op) or a pointer
/// previously returned by [`alloc_render_settings`] that hasn't already
/// been freed, paired with the exact `size` it was allocated with.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_render_settings(ptr: *mut u8, size: usize) {
    // SAFETY: the caller upholds this function's safety contract.
    unsafe { free_bytes(ptr, size) };
}

/// Allocate `size` bytes inside this module's own memory for the host to
/// write an interpreter-settings JSON blob into (see [`render_page`] for
/// the shape), and return a pointer to it. `size` must be the exact byte
/// length of the JSON text that will be written.
#[unsafe(no_mangle)]
pub extern "C" fn alloc_interpreter_settings(size: usize) -> *mut u8 {
    alloc_bytes(size)
}

/// Free a buffer previously returned by [`alloc_interpreter_settings`].
/// `size` must be the exact size that was originally allocated.
///
/// # Safety
/// `ptr` must be null (in which case this is a no-op) or a pointer
/// previously returned by [`alloc_interpreter_settings`] that hasn't
/// already been freed, paired with the exact `size` it was allocated with.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_interpreter_settings(ptr: *mut u8, size: usize) {
    // SAFETY: the caller upholds this function's safety contract.
    unsafe { free_bytes(ptr, size) };
}

/// Allocate a zeroed 4-byte `u32` cell, for use as one of [`render_page`]'s
/// `width_out`/`height_out` arguments.
#[unsafe(no_mangle)]
pub extern "C" fn alloc_u32() -> *mut u32 {
    alloc_bytes(4).cast()
}

/// Free a cell previously returned by [`alloc_u32`].
///
/// # Safety
/// `ptr` must be null (in which case this is a no-op) or a pointer
/// previously returned by [`alloc_u32`] that hasn't already been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_u32(ptr: *mut u32) {
    // SAFETY: the caller upholds this function's safety contract; `ptr` was
    // allocated with `layout_for(4)`.
    unsafe { free_bytes(ptr.cast(), 4) };
}

fn layout_for(size: usize) -> Layout {
    // Byte buffers have no particular alignment requirement, so alignment 1
    // is fine (and matches what we allocated with).
    Layout::from_size_align(size, 1).expect("size does not overflow isize::MAX")
}

/// Allocate `size` bytes inside this module's own memory, or a null
/// pointer if `size` is `0` or the allocation failed.
fn alloc_bytes(size: usize) -> *mut u8 {
    if size == 0 {
        return std::ptr::null_mut();
    }

    // SAFETY: checked `size != 0` above, the one precondition `alloc` has.
    unsafe { alloc(layout_for(size)) }
}

/// Free a buffer previously returned by [`alloc_bytes`] with the same
/// `size`.
///
/// # Safety
/// `ptr` must be null (in which case this is a no-op) or a pointer
/// previously returned by [`alloc_bytes`] that hasn't already been freed,
/// paired with the exact `size` it was allocated with.
unsafe fn free_bytes(ptr: *mut u8, size: usize) {
    if ptr.is_null() || size == 0 {
        return;
    }

    // SAFETY: the caller upholds this function's safety contract; `ptr` was
    // allocated using `layout_for(size)`.
    unsafe { dealloc(ptr, layout_for(size)) };
}

/// Parse a PDF and return its page count, or `-1` if it could not be parsed.
///
/// `pdf_ptr`/`pdf_len` describe a buffer previously written via
/// [`alloc_pdf`].
///
/// # Safety
/// `pdf_ptr`/`pdf_len` must describe a live, initialized buffer of
/// `pdf_len` bytes — e.g. one obtained from [`alloc_pdf`] and fully written
/// by the host.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn page_count(pdf_ptr: *const u8, pdf_len: usize) -> i32 {
    // SAFETY: the caller upholds this function's safety contract.
    let Some(pdf) = (unsafe { parse_pdf(pdf_ptr, pdf_len) }) else {
        return -1;
    };

    pdf.pages().len() as i32
}

/// Render one page of a PDF to a raw RGBA8 pixmap.
///
/// `page_number` is **1-based**.
///
/// `interpreter_settings_ptr`/`interpreter_settings_len` and
/// `render_settings_ptr`/`render_settings_len` each independently select a
/// `hayro` settings struct to use for this render, as UTF-8 JSON text. Pass
/// a null pointer (with a length of `0`) for either to use all of
/// `hayro`'s defaults for that struct; otherwise every field in the JSON
/// object is itself optional, and an absent field means "use the default
/// for just this field". Malformed JSON, or an object with an unrecognized
/// field name, is treated as a failure the same as any other — see the
/// return value below.
///
/// **Render-settings JSON** (mirrors `hayro::RenderSettings` field-for-field
/// except `bg_color`), all fields optional:
/// - `x_scale`, `y_scale`: numbers, `hayro`'s default is `1.0` for each.
///   Unlike the old fixed-byte-layout version of this crate, an explicit
///   `0.0` is honored literally (and, like any other zero scale, produces
///   a zero-area — and thus failing — render) rather than being
///   reinterpreted as "use the default".
/// - `width`, `height`: integers in `0..=65535`. Absent means "auto".
/// - `bg_color`: an object `{"r": .., "g": .., "b": .., "a": ..}`, each
///   `0..=255`, **straight (non-premultiplied) alpha** — this matches Go's
///   `image/color.NRGBA`, *not* `color.RGBA` (which is alpha-premultiplied).
///   Absent means `hayro`'s actual default (i.e. `#00000000` — fully
///   transparent black).
///
/// **Interpreter-settings JSON** (mirrors one field of
/// `hayro_interpret::InterpreterSettings`), one optional field:
/// - `render_annotations`: boolean. Absent means "use `hayro`'s default"
///   (not hardcoded — read from `InterpreterSettings::default()` at call
///   time, so this stays correct even if that default ever changes).
///
/// `InterpreterSettings` also has `font_resolver`, `cmap_resolver`, and
/// `warning_sink` fields, which are Rust closures and have no plain-bytes
/// representation, so this bridge can't expose them — doing so would mean
/// this module importing host-provided callback functions, which is a
/// deliberate design decision of its own, not just another JSON field (see
/// the crate README's "zero required host imports" goal). They're left at
/// `hayro`'s defaults, which are still meaningful: this crate enables
/// `hayro`'s `embed-fonts`/`embed-cmaps` features, so the standard 14 fonts
/// and predefined cmaps resolve from embedded data even without a callback.
///
/// `width_out`/`height_out` must each point to a 4-byte `u32` cell
/// (obtained from [`alloc_u32`]); on success this function writes the
/// rendered pixel width/height into them. They're left unwritten if this
/// function returns a null pointer.
///
/// Returns a pointer to `width * height * 4` bytes of RGBA8 pixel data
/// (non-premultiplied, one byte per channel), or a null pointer (`0`) on
/// failure — an unparseable PDF, an out-of-range `page_number`, malformed
/// settings JSON, or a render that came out zero-area.
///
/// The caller must eventually free a non-null result with [`free_pixels`],
/// passing the same `width`/`height` this function wrote to `width_out`/
/// `height_out`.
///
/// # Safety
/// `pdf_ptr`/`pdf_len` must describe a live, initialized buffer, as required
/// by [`page_count`]. `interpreter_settings_ptr`/`render_settings_ptr` must
/// each be null (with a length of `0`) or point to a live, initialized
/// buffer of the given length. `width_out`/`height_out` must each point to
/// a live, writable 4-byte `u32` cell.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn render_page(
    pdf_ptr: *const u8,
    pdf_len: usize,
    page_number: u32,
    interpreter_settings_ptr: *const u8,
    interpreter_settings_len: usize,
    render_settings_ptr: *const u8,
    render_settings_len: usize,
    width_out: *mut u32,
    height_out: *mut u32,
) -> *mut u8 {
    // SAFETY: the caller upholds this function's safety contract.
    let Some(pdf) = (unsafe { parse_pdf(pdf_ptr, pdf_len) }) else {
        return std::ptr::null_mut();
    };
    let Some(page) = page_number
        .checked_sub(1)
        .and_then(|idx| pdf.pages().get(idx as usize))
    else {
        return std::ptr::null_mut();
    };

    // SAFETY: the host is required to pass either a null pointer (with a
    // length of 0) or one pointing to a live, initialized buffer of the
    // given length.
    let Ok(interpreter_settings) =
        (unsafe { read_interpreter_settings(interpreter_settings_ptr, interpreter_settings_len) })
    else {
        return std::ptr::null_mut();
    };
    // SAFETY: same requirement as above.
    let Ok(render_settings) =
        (unsafe { read_render_settings(render_settings_ptr, render_settings_len) })
    else {
        return std::ptr::null_mut();
    };

    let cache = RenderCache::new();
    let pixmap = hayro::render(page, &cache, &interpreter_settings, &render_settings);

    let width = pixmap.width() as u32;
    let height = pixmap.height() as u32;
    let rgba: Vec<u8> = bytemuck::cast_vec(pixmap.take_unpremultiplied());

    // `alloc` requires a non-zero size, and a zero-area render (e.g. an
    // explicit 0 scale) isn't a useful result anyway, so it's treated the
    // same as any other failure: null, with the output cells left
    // untouched.
    if rgba.is_empty() {
        return std::ptr::null_mut();
    }

    // SAFETY: `rgba.len()` is non-zero, just checked above.
    let out = unsafe { alloc(layout_for(rgba.len())) };
    if out.is_null() {
        return std::ptr::null_mut();
    }

    // SAFETY: `out` was just allocated with `rgba.len()` bytes.
    unsafe { std::ptr::copy_nonoverlapping(rgba.as_ptr(), out, rgba.len()) };

    // SAFETY: the host is required to pass pointers to live, writable 4-byte
    // `u32` cells for both of these.
    unsafe {
        *width_out = width;
        *height_out = height;
    }

    out
}

/// Free a pixel buffer previously returned by [`render_page`]. `width`/
/// `height` must be the exact values [`render_page`] wrote into its
/// `width_out`/`height_out` arguments for that call.
///
/// # Safety
/// `ptr` must be null (in which case this is a no-op) or a pointer
/// previously returned by [`render_page`], together with the `width`/
/// `height` it reported for that call, that hasn't already been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_pixels(ptr: *mut u8, width: u32, height: u32) {
    let len = width as usize * height as usize * 4;
    // SAFETY: the caller upholds this function's safety contract; `ptr` was
    // allocated with `layout_for(width * height * 4)`.
    unsafe { free_bytes(ptr, len) };
}

/// Read a [`RenderSettings`] from the JSON object at `ptr`/`len`, or
/// `RenderSettings::default()` if `ptr` is null. `Err` means the bytes at
/// `ptr`/`len` were not valid JSON matching [`RenderSettingsJson`]'s shape
/// — the caller should treat this as a hard failure, not silently fall
/// back to defaults, since it most likely means the host and this module
/// have drifted out of sync about the settings' shape.
///
/// # Safety
/// If non-null, `ptr` must point to a live, initialized buffer of exactly
/// `len` bytes — see [`render_page`]'s docs for the JSON shape.
unsafe fn read_render_settings(
    ptr: *const u8,
    len: usize,
) -> Result<RenderSettings, serde_json::Error> {
    if ptr.is_null() {
        return Ok(RenderSettings::default());
    }

    // SAFETY: the caller upholds this function's safety contract.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    let json: RenderSettingsJson = serde_json::from_slice(bytes)?;

    let defaults = RenderSettings::default();
    Ok(RenderSettings {
        x_scale: json.x_scale.unwrap_or(defaults.x_scale),
        y_scale: json.y_scale.unwrap_or(defaults.y_scale),
        width: json.width,
        height: json.height,
        bg_color: json
            .bg_color
            .map(|RgbaJson { r, g, b, a }| AlphaColor::from_rgba8(r, g, b, a))
            .unwrap_or(defaults.bg_color),
    })
}

/// Read the subset of [`InterpreterSettings`] this bridge exposes from the
/// JSON object at `ptr`/`len`, or `InterpreterSettings::default()` if `ptr`
/// is null. `Err` means the bytes at `ptr`/`len` were not valid JSON
/// matching [`InterpreterSettingsJson`]'s shape — treated as a hard
/// failure, same as [`read_render_settings`].
///
/// # Safety
/// If non-null, `ptr` must point to a live, initialized buffer of exactly
/// `len` bytes — see [`render_page`]'s docs for the JSON shape.
unsafe fn read_interpreter_settings(
    ptr: *const u8,
    len: usize,
) -> Result<InterpreterSettings, serde_json::Error> {
    if ptr.is_null() {
        return Ok(InterpreterSettings::default());
    }

    // SAFETY: the caller upholds this function's safety contract.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    let json: InterpreterSettingsJson = serde_json::from_slice(bytes)?;

    let defaults = InterpreterSettings::default();
    Ok(InterpreterSettings {
        render_annotations: json
            .render_annotations
            .unwrap_or(defaults.render_annotations),
        ..defaults
    })
}

/// Read `pdf_len` bytes starting at `pdf_ptr` and try to parse them as a PDF.
///
/// # Safety
/// `pdf_ptr`/`pdf_len` must describe a live, initialized buffer of
/// `pdf_len` bytes — e.g. one obtained from [`alloc_pdf`] and fully written
/// by the host.
unsafe fn parse_pdf(pdf_ptr: *const u8, pdf_len: usize) -> Option<Pdf> {
    // SAFETY: the caller upholds this function's safety contract. We copy
    // the bytes out immediately rather than holding onto the borrow.
    let bytes = unsafe { std::slice::from_raw_parts(pdf_ptr, pdf_len) }.to_vec();

    Pdf::new(bytes).ok()
}
