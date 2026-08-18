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
//! Calling convention, from the host's side:
//! 1. Call [`wasm_alloc`] to reserve space, and write the PDF's bytes into
//!    the module's memory at the returned offset.
//! 2. Call [`render_page`] with that offset/length. It returns a pointer to
//!    a result buffer (or `0` = null on failure).
//! 3. Read the result: first 4 bytes = width (u32 little-endian), next 4
//!    bytes = height (u32 little-endian), remainder = `width * height * 4`
//!    bytes of RGBA8 pixel data (non-premultiplied, one byte per channel).
//! 4. Free both buffers with [`wasm_free`] once you're done with them.

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::{RenderCache, RenderSettings};
use std::alloc::{Layout, alloc, dealloc};

/// Number of header bytes ([width][height]) placed before the pixel data in
/// the buffer [`render_page`] returns.
const HEADER_LEN: usize = 8;

/// Allocate `size` bytes inside this module's own memory and return a
/// pointer to the start of it.
///
/// The host calls this *before* a call like [`render_page`] that needs
/// input bytes handed to it — e.g. allocate a buffer as big as the PDF
/// file, copy the file's bytes into the module's memory at the returned
/// offset, then pass that offset in.
///
/// Every pointer this crate hands back (from here, or as a `render_page`
/// result) must eventually be passed to [`wasm_free`], with the exact same
/// `size` used to allocate it — Rust's allocator needs that size again to
/// free the memory correctly; there's no separate bookkeeping of "how big
/// was this block" the way libc's `malloc`/`free` do it for you.
#[unsafe(no_mangle)]
pub extern "C" fn wasm_alloc(size: usize) -> *mut u8 {
    if size == 0 {
        return std::ptr::null_mut();
    }

    // SAFETY: `layout_for(size)` is non-zero size, the one precondition
    // `alloc` has.
    unsafe { alloc(layout_for(size)) }
}

/// Free memory previously returned by [`wasm_alloc`] or [`render_page`].
/// `size` must be the exact size that was originally allocated.
#[unsafe(no_mangle)]
pub extern "C" fn wasm_free(ptr: *mut u8, size: usize) {
    if ptr.is_null() || size == 0 {
        return;
    }

    // SAFETY: `ptr` was allocated by `wasm_alloc`/`render_page` using
    // `layout_for(size)`, and this is the only place either of them is freed.
    unsafe { dealloc(ptr, layout_for(size)) };
}

fn layout_for(size: usize) -> Layout {
    // Byte buffers have no particular alignment requirement, so alignment 1
    // is fine (and matches what we allocated with).
    Layout::from_size_align(size, 1).expect("size does not overflow isize::MAX")
}

/// Parse a PDF and return its page count, or `-1` if it could not be parsed.
///
/// `pdf_ptr`/`pdf_len` describe a buffer previously written via
/// [`wasm_alloc`].
#[unsafe(no_mangle)]
pub extern "C" fn page_count(pdf_ptr: *const u8, pdf_len: usize) -> i32 {
    let Some(pdf) = parse_pdf(pdf_ptr, pdf_len) else {
        return -1;
    };

    pdf.pages().len() as i32
}

/// Render one page of a PDF to a raw RGBA8 pixmap.
///
/// `page_number` is **1-based**. `scale` of `1.0` renders at the PDF's
/// native size (72 points per inch); `2.0` doubles both dimensions, etc.
///
/// Returns a pointer to a buffer laid out as:
/// - bytes `[0..4)`: pixel width, `u32` little-endian
/// - bytes `[4..8)`: pixel height, `u32` little-endian
/// - bytes `[8..)`: `width * height * 4` bytes of RGBA8 pixel data
///
/// Returns a null pointer (`0`) on failure — an unparseable PDF or an
/// out-of-range `page_number`.
///
/// The caller must eventually free a non-null result with [`wasm_free`],
/// passing `8 + width * height * 4` as the size.
#[unsafe(no_mangle)]
pub extern "C" fn render_page(
    pdf_ptr: *const u8,
    pdf_len: usize,
    page_number: u32,
    scale: f32,
) -> *mut u8 {
    let Some(pdf) = parse_pdf(pdf_ptr, pdf_len) else {
        return std::ptr::null_mut();
    };
    let Some(page) = page_number
        .checked_sub(1)
        .and_then(|idx| pdf.pages().get(idx as usize))
    else {
        return std::ptr::null_mut();
    };

    let render_settings = RenderSettings {
        x_scale: scale,
        y_scale: scale,
        ..Default::default()
    };
    let cache = RenderCache::new();
    let pixmap = hayro::render(
        page,
        &cache,
        &InterpreterSettings::default(),
        &render_settings,
    );

    let width = pixmap.width() as u32;
    let height = pixmap.height() as u32;
    let rgba: Vec<u8> = bytemuck::cast_vec(pixmap.take_unpremultiplied());

    let total_len = HEADER_LEN + rgba.len();
    // SAFETY: `total_len` is non-zero (header alone is 8 bytes).
    let out = unsafe { alloc(layout_for(total_len)) };
    if out.is_null() {
        return std::ptr::null_mut();
    }

    // SAFETY: `out` was just allocated with `total_len` bytes, and none of
    // these three writes overlap with each other or run past the end of it.
    unsafe {
        std::ptr::copy_nonoverlapping(width.to_le_bytes().as_ptr(), out, 4);
        std::ptr::copy_nonoverlapping(height.to_le_bytes().as_ptr(), out.add(4), 4);
        std::ptr::copy_nonoverlapping(rgba.as_ptr(), out.add(HEADER_LEN), rgba.len());
    }

    out
}

/// Read `pdf_len` bytes starting at `pdf_ptr` and try to parse them as a PDF.
fn parse_pdf(pdf_ptr: *const u8, pdf_len: usize) -> Option<Pdf> {
    // SAFETY: the host is required to have handed us a pointer/length that
    // describe a live, initialized buffer of `pdf_len` bytes (one it got
    // from `wasm_alloc` and wrote `pdf_len` bytes into). We copy the bytes
    // out immediately rather than holding onto the borrow.
    let bytes = unsafe { std::slice::from_raw_parts(pdf_ptr, pdf_len) }.to_vec();

    Pdf::new(bytes).ok()
}
