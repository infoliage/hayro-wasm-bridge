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
//! than one generic `alloc(size)`/`free(ptr, size)` pair. For the
//! fixed-size kinds this means the host never has to know or pass back a
//! size at all — the function you call *is* the size, so a mismatch
//! between what a buffer was allocated with and what it's freed with isn't
//! expressible, and it can't drift out of sync with a newer/older build of
//! this module the way a host-hardcoded size constant could.
//!
//! Calling convention, from the host's side:
//! 1. Call [`alloc_pdf`] to reserve space for the PDF's bytes, and write
//!    them into the module's memory at the returned offset.
//! 2. Optionally call [`alloc_render_settings`] and/or
//!    [`alloc_interpreter_settings`] and write into the result, if you want
//!    anything other than `hayro`'s defaults — see [`render_page`] for the
//!    layout of each. Both buffers start zeroed, and every field's `0`
//!    value was deliberately chosen to mean "use the default", so it's fine
//!    to only write the fields you actually care about.
//! 3. Call [`alloc_u32`] twice, for [`render_page`]'s `width_out`/
//!    `height_out` arguments.
//! 4. Call [`render_page`]. It returns a pointer to `width * height * 4`
//!    bytes of RGBA8 pixel data (non-premultiplied, one byte per channel),
//!    or `0` = null on failure.
//! 5. Free everything you allocated: [`free_pdf`], [`free_render_settings`]/
//!    [`free_interpreter_settings`] if you used them, [`free_u32`] (for
//!    each of the two output cells), and [`free_pixels`] for the result.

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::AlphaColor;
use hayro::{RenderCache, RenderSettings};
use std::alloc::{Layout, alloc, alloc_zeroed, dealloc};

#[cfg(test)]
mod tests;

/// Byte length of a render-settings blob. See [`render_page`]'s docs for
/// the exact layout.
const RENDER_SETTINGS_LEN: usize = 16;

/// Byte length of an interpreter-settings blob. See [`render_page`]'s docs
/// for the exact layout.
const INTERPRETER_SETTINGS_LEN: usize = 1;

/// Allocate `size` bytes inside this module's own memory for the host to
/// write a PDF file's bytes into, and return a pointer to the start of it.
///
/// Every pointer this function hands back must eventually be passed to
/// [`free_pdf`], with the exact same `size` used to allocate it — Rust's
/// allocator needs that size again to free the memory correctly; there's no
/// separate bookkeeping of "how big was this block" the way libc's
/// `malloc`/`free` do it for you. (The fixed-size allocation pairs below
/// don't have this requirement — there, the size is implied by which
/// function you called.)
#[unsafe(no_mangle)]
pub extern "C" fn alloc_pdf(size: usize) -> *mut u8 {
    if size == 0 {
        return std::ptr::null_mut();
    }

    // SAFETY: checked `size != 0` above, the one precondition `alloc` has.
    unsafe { alloc(layout_for(size)) }
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
    if ptr.is_null() || size == 0 {
        return;
    }

    // SAFETY: the caller upholds this function's safety contract; `ptr` was
    // allocated using `layout_for(size)`.
    unsafe { dealloc(ptr, layout_for(size)) };
}

/// Allocate a zeroed [`RENDER_SETTINGS_LEN`]-byte buffer for a
/// render-settings blob (see [`render_page`]'s docs for the exact layout),
/// and return a pointer to it.
#[unsafe(no_mangle)]
pub extern "C" fn alloc_render_settings() -> *mut u8 {
    alloc_zeroed_bytes(RENDER_SETTINGS_LEN)
}

/// Free a buffer previously returned by [`alloc_render_settings`].
///
/// # Safety
/// `ptr` must be null (in which case this is a no-op) or a pointer
/// previously returned by [`alloc_render_settings`] that hasn't already
/// been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_render_settings(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }

    // SAFETY: the caller upholds this function's safety contract; `ptr` was
    // allocated with `layout_for(RENDER_SETTINGS_LEN)`.
    unsafe { dealloc(ptr, layout_for(RENDER_SETTINGS_LEN)) };
}

/// Allocate a zeroed [`INTERPRETER_SETTINGS_LEN`]-byte buffer for an
/// interpreter-settings blob (see [`render_page`]'s docs for the exact
/// layout), and return a pointer to it.
#[unsafe(no_mangle)]
pub extern "C" fn alloc_interpreter_settings() -> *mut u8 {
    alloc_zeroed_bytes(INTERPRETER_SETTINGS_LEN)
}

/// Free a buffer previously returned by [`alloc_interpreter_settings`].
///
/// # Safety
/// `ptr` must be null (in which case this is a no-op) or a pointer
/// previously returned by [`alloc_interpreter_settings`] that hasn't
/// already been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_interpreter_settings(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }

    // SAFETY: the caller upholds this function's safety contract; `ptr` was
    // allocated with `layout_for(INTERPRETER_SETTINGS_LEN)`.
    unsafe { dealloc(ptr, layout_for(INTERPRETER_SETTINGS_LEN)) };
}

/// Allocate a zeroed 4-byte `u32` cell, for use as one of [`render_page`]'s
/// `width_out`/`height_out` arguments.
#[unsafe(no_mangle)]
pub extern "C" fn alloc_u32() -> *mut u32 {
    alloc_zeroed_bytes(4).cast()
}

/// Free a cell previously returned by [`alloc_u32`].
///
/// # Safety
/// `ptr` must be null (in which case this is a no-op) or a pointer
/// previously returned by [`alloc_u32`] that hasn't already been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_u32(ptr: *mut u32) {
    if ptr.is_null() {
        return;
    }

    // SAFETY: the caller upholds this function's safety contract; `ptr` was
    // allocated with `layout_for(4)`.
    unsafe { dealloc(ptr.cast(), layout_for(4)) };
}

fn layout_for(size: usize) -> Layout {
    // Byte buffers have no particular alignment requirement, so alignment 1
    // is fine (and matches what we allocated with).
    Layout::from_size_align(size, 1).expect("size does not overflow isize::MAX")
}

/// Allocate `size` zeroed bytes inside this module's own memory, or a null
/// pointer if `size` is `0` or the allocation failed.
fn alloc_zeroed_bytes(size: usize) -> *mut u8 {
    if size == 0 {
        return std::ptr::null_mut();
    }

    // SAFETY: checked `size != 0` above, the one precondition `alloc_zeroed`
    // has.
    unsafe { alloc_zeroed(layout_for(size)) }
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
/// `interpreter_settings_ptr` and `render_settings_ptr` each independently
/// select a `hayro` settings struct to use for this render. Pass `0` (null)
/// for either to use `hayro`'s defaults; otherwise it must point to a
/// buffer of the exact length below, obtained from [`alloc_render_settings`]
/// / [`alloc_interpreter_settings`] respectively. Partial blobs aren't
/// supported — a non-null pointer must have the whole documented length
/// behind it.
///
/// **Render-settings blob** (mirrors `hayro::RenderSettings` field-for-field
/// except `bg_color`), [`RENDER_SETTINGS_LEN`] = 16 bytes:
/// - bytes `[0..4)`: `x_scale`, `f32` little-endian; `0.0` means "use the
///   default" (`1.0`) — an explicit `0.0` scale would always produce a
///   zero-area (and thus failing) render anyway, so nothing is lost by
///   giving it this meaning instead, and it's what makes a zeroed blob (see
///   [`alloc_render_settings`]) behave exactly like `RenderSettings::default()`
/// - bytes `[4..8)`: `y_scale`, `f32` little-endian; same `0.0` meaning as
///   `x_scale`
/// - bytes `[8..10)`: `width`, `u16` little-endian; `0` means "auto" (`None`)
/// - bytes `[10..12)`: `height`, `u16` little-endian; `0` means "auto"
///   (`None`)
/// - bytes `[12..16)`: `bg_color` as R, G, B, A (`0..255` each), **straight
///   (non-premultiplied) alpha** — this matches Go's `image/color.NRGBA`,
///   *not* `color.RGBA` (which is alpha-premultiplied). All-zero means fully
///   transparent black, `hayro`'s actual default.
///
/// **Interpreter-settings blob** (mirrors one field of
/// `hayro_interpret::InterpreterSettings`), [`INTERPRETER_SETTINGS_LEN`] = 1
/// byte:
/// - byte `[0..1)`: `render_annotations`; `0` = use `hayro`'s default (not
///   hardcoded — read from `InterpreterSettings::default()` at call time,
///   so this stays correct even if that default ever changes), `1` =
///   enabled, anything else = disabled
///
/// `InterpreterSettings` also has `font_resolver`, `cmap_resolver`, and
/// `warning_sink` fields, which are Rust closures and have no plain-bytes
/// representation, so this bridge can't expose them — doing so would mean
/// this module importing host-provided callback functions, which is a
/// deliberate design decision of its own, not just another blob field (see
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
/// failure — an unparseable PDF, an out-of-range `page_number`, or a render
/// that came out zero-area.
///
/// The caller must eventually free a non-null result with [`free_pixels`],
/// passing the same `width`/`height` this function wrote to `width_out`/
/// `height_out`.
///
/// # Safety
/// `pdf_ptr`/`pdf_len` must describe a live, initialized buffer, as required
/// by [`page_count`]. `interpreter_settings_ptr`/`render_settings_ptr` must
/// each be null or point to a live, initialized buffer of the length
/// documented above. `width_out`/`height_out` must each point to a live,
/// writable 4-byte `u32` cell.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn render_page(
    pdf_ptr: *const u8,
    pdf_len: usize,
    page_number: u32,
    interpreter_settings_ptr: *const u8,
    render_settings_ptr: *const u8,
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

    // SAFETY: the host is required to pass either a null pointer or one
    // pointing to a live, initialized buffer of the length documented above.
    let interpreter_settings = unsafe { read_interpreter_settings(interpreter_settings_ptr) };
    // SAFETY: same requirement as above.
    let render_settings = unsafe { read_render_settings(render_settings_ptr) };

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
    if ptr.is_null() || len == 0 {
        return;
    }

    // SAFETY: the caller upholds this function's safety contract; `ptr` was
    // allocated with `layout_for(width * height * 4)`.
    unsafe { dealloc(ptr, layout_for(len)) };
}

/// Read a [`RenderSettings`] from `ptr`, or `RenderSettings::default()` if
/// `ptr` is null.
///
/// # Safety
/// If non-null, `ptr` must point to a live, initialized buffer of at least
/// [`RENDER_SETTINGS_LEN`] bytes — see [`render_page`]'s docs for the exact
/// layout.
unsafe fn read_render_settings(ptr: *const u8) -> RenderSettings {
    if ptr.is_null() {
        return RenderSettings::default();
    }

    // SAFETY: the caller upholds this function's safety contract.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, RENDER_SETTINGS_LEN) };
    let x_scale = f32::from_le_bytes(bytes[0..4].try_into().unwrap());
    let y_scale = f32::from_le_bytes(bytes[4..8].try_into().unwrap());
    let width = u16::from_le_bytes(bytes[8..10].try_into().unwrap());
    let height = u16::from_le_bytes(bytes[10..12].try_into().unwrap());
    let [r, g, b, a] = [bytes[12], bytes[13], bytes[14], bytes[15]];

    let defaults = RenderSettings::default();

    RenderSettings {
        x_scale: if x_scale == 0.0 {
            defaults.x_scale
        } else {
            x_scale
        },
        y_scale: if y_scale == 0.0 {
            defaults.y_scale
        } else {
            y_scale
        },
        width: (width != 0).then_some(width),
        height: (height != 0).then_some(height),
        bg_color: AlphaColor::from_rgba8(r, g, b, a),
    }
}

/// Read the subset of [`InterpreterSettings`] this bridge exposes from
/// `ptr`, or `InterpreterSettings::default()` if `ptr` is null.
///
/// # Safety
/// If non-null, `ptr` must point to a live, initialized buffer of at least
/// [`INTERPRETER_SETTINGS_LEN`] bytes — see [`render_page`]'s docs for the
/// exact layout.
unsafe fn read_interpreter_settings(ptr: *const u8) -> InterpreterSettings {
    if ptr.is_null() {
        return InterpreterSettings::default();
    }

    // SAFETY: the caller upholds this function's safety contract.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, INTERPRETER_SETTINGS_LEN) };

    let defaults = InterpreterSettings::default();
    let render_annotations = match bytes[0] {
        // `0` defers to whatever `hayro`'s actual current default is,
        // queried here rather than hardcoded, so this stays correct even
        // if that default ever changes.
        0 => defaults.render_annotations,
        1 => true,
        _ => false,
    };

    InterpreterSettings {
        render_annotations,
        ..defaults
    }
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
