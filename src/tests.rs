//! Tests for the C-ABI boundary in `lib.rs`.
//!
//! These run against the native host target via plain `cargo test` — they
//! call the `extern "C"` functions directly, in-process, rather than going
//! through an actual wasm runtime. That's enough to cover essentially all of
//! this crate's own logic (blob decoding, alloc/free pairing, null/failure
//! handling); it's `hayro` itself, not this bridge, that's responsible for
//! rendering correctness, and that's covered by `hayro`'s own test suite.
//!
//! Fixture PDFs are hand-crafted byte constants rather than external files,
//! to keep this crate free of test-asset dependencies. `MINIMAL_PDF`'s xref
//! table is deliberately a stub (`hayro` reconstructs it, same as it does
//! for real-world malformed PDFs) — this was checked against a real `hayro`
//! render while writing these tests: it parses to 1 page and renders to a
//! 200x100 pixmap with visible (non-blank) content.

use super::*;

/// A single 200x100pt page with "Hello World" in Helvetica at roughly
/// x: 20..180, y: 40..64 in PDF space (bottom-up) — i.e. away from every
/// edge of the page, so corner pixels are reliably pure background.
const MINIMAL_PDF: &[u8] = b"%PDF-1.1
1 0 obj
<< /Type /Catalog /Pages 2 0 R >>
endobj
2 0 obj
<< /Type /Pages /Kids [3 0 R] /Count 1 >>
endobj
3 0 obj
<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>
endobj
4 0 obj
<< /Length 58 >>
stream
BT /F1 24 Tf 20 40 Td (Hello World) Tj ET
endstream
endobj
5 0 obj
<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>
endobj
xref
0 6
0000000000 65535 f
trailer
<< /Size 6 /Root 1 0 R >>
startxref
0
%%EOF";

/// Build a [`RENDER_SETTINGS_LEN`]-byte render-settings blob, per the layout
/// documented on [`render_page`].
fn render_settings_blob(x_scale: f32, y_scale: f32, width: u16, height: u16, rgba: [u8; 4]) -> [u8; RENDER_SETTINGS_LEN] {
    let mut blob = [0u8; RENDER_SETTINGS_LEN];
    blob[0..4].copy_from_slice(&x_scale.to_le_bytes());
    blob[4..8].copy_from_slice(&y_scale.to_le_bytes());
    blob[8..10].copy_from_slice(&width.to_le_bytes());
    blob[10..12].copy_from_slice(&height.to_le_bytes());
    blob[12..16].copy_from_slice(&rgba);
    blob
}

// ---- Render-settings blob decoding ----------------------------------------

#[test]
fn render_settings_null_ptr_is_default() {
    let settings = unsafe { read_render_settings(std::ptr::null()) };
    let defaults = RenderSettings::default();
    assert_eq!(settings.x_scale, defaults.x_scale);
    assert_eq!(settings.y_scale, defaults.y_scale);
    assert_eq!(settings.width, defaults.width);
    assert_eq!(settings.height, defaults.height);
    assert_eq!(settings.bg_color.to_rgba8(), defaults.bg_color.to_rgba8());
}

#[test]
fn render_settings_zeroed_blob_is_default() {
    // The whole point of every field's "0 means default" convention: a
    // freshly `alloc_render_settings`'d (zeroed) blob must behave exactly
    // like a null pointer / `RenderSettings::default()`.
    let blob = [0u8; RENDER_SETTINGS_LEN];
    let settings = unsafe { read_render_settings(blob.as_ptr()) };
    let defaults = RenderSettings::default();
    assert_eq!(settings.x_scale, defaults.x_scale);
    assert_eq!(settings.y_scale, defaults.y_scale);
    assert_eq!(settings.width, defaults.width);
    assert_eq!(settings.height, defaults.height);
    assert_eq!(settings.bg_color.to_rgba8(), defaults.bg_color.to_rgba8());
}

#[test]
fn render_settings_decodes_explicit_values() {
    let blob = render_settings_blob(2.5, 3.5, 800, 600, [10, 20, 30, 128]);
    let settings = unsafe { read_render_settings(blob.as_ptr()) };
    assert_eq!(settings.x_scale, 2.5);
    assert_eq!(settings.y_scale, 3.5);
    assert_eq!(settings.width, Some(800));
    assert_eq!(settings.height, Some(600));
    assert_eq!(
        settings.bg_color.to_rgba8(),
        AlphaColor::from_rgba8(10, 20, 30, 128).to_rgba8()
    );
}

// ---- Interpreter-settings blob decoding ------------------------------------

#[test]
fn interpreter_settings_null_ptr_is_default() {
    let settings = unsafe { read_interpreter_settings(std::ptr::null()) };
    assert_eq!(
        settings.render_annotations,
        InterpreterSettings::default().render_annotations
    );
}

#[test]
fn interpreter_settings_zero_byte_is_default() {
    let blob = [0u8; INTERPRETER_SETTINGS_LEN];
    let settings = unsafe { read_interpreter_settings(blob.as_ptr()) };
    assert_eq!(
        settings.render_annotations,
        InterpreterSettings::default().render_annotations
    );
}

#[test]
fn interpreter_settings_one_byte_enables_annotations() {
    let blob = [1u8; INTERPRETER_SETTINGS_LEN];
    let settings = unsafe { read_interpreter_settings(blob.as_ptr()) };
    assert!(settings.render_annotations);
}

#[test]
fn interpreter_settings_other_bytes_disable_annotations() {
    for byte in [2u8, 255u8] {
        let blob = [byte; INTERPRETER_SETTINGS_LEN];
        let settings = unsafe { read_interpreter_settings(blob.as_ptr()) };
        assert!(!settings.render_annotations, "byte {byte} should disable annotations");
    }
}

// ---- Alloc/free pairs -------------------------------------------------------

#[test]
fn alloc_pdf_zero_size_returns_null() {
    assert!(alloc_pdf(0).is_null());
}

#[test]
fn free_pdf_null_is_noop() {
    unsafe { free_pdf(std::ptr::null_mut(), 0) };
    unsafe { free_pdf(std::ptr::null_mut(), 16) };
}

#[test]
fn alloc_pdf_round_trip() {
    let ptr = alloc_pdf(8);
    assert!(!ptr.is_null());
    unsafe { free_pdf(ptr, 8) };
}

#[test]
fn alloc_render_settings_is_zeroed() {
    let ptr = alloc_render_settings();
    assert!(!ptr.is_null());
    let bytes = unsafe { std::slice::from_raw_parts(ptr, RENDER_SETTINGS_LEN) };
    assert!(bytes.iter().all(|&b| b == 0));
    unsafe { free_render_settings(ptr) };
}

#[test]
fn free_render_settings_null_is_noop() {
    unsafe { free_render_settings(std::ptr::null_mut()) };
}

#[test]
fn alloc_interpreter_settings_is_zeroed() {
    let ptr = alloc_interpreter_settings();
    assert!(!ptr.is_null());
    let bytes = unsafe { std::slice::from_raw_parts(ptr, INTERPRETER_SETTINGS_LEN) };
    assert!(bytes.iter().all(|&b| b == 0));
    unsafe { free_interpreter_settings(ptr) };
}

#[test]
fn free_interpreter_settings_null_is_noop() {
    unsafe { free_interpreter_settings(std::ptr::null_mut()) };
}

#[test]
fn alloc_u32_is_zeroed() {
    let ptr = alloc_u32();
    assert!(!ptr.is_null());
    assert_eq!(unsafe { *ptr }, 0);
    unsafe { free_u32(ptr) };
}

#[test]
fn free_u32_null_is_noop() {
    unsafe { free_u32(std::ptr::null_mut()) };
}

#[test]
fn free_pixels_null_is_noop() {
    unsafe { free_pixels(std::ptr::null_mut(), 0, 0) };
    unsafe { free_pixels(std::ptr::null_mut(), 100, 100) };
}

#[test]
fn free_pixels_zero_len_does_not_free_a_real_buffer() {
    // width * height == 0 must make `free_pixels` short-circuit before ever
    // calling `dealloc` — so freeing `ptr` correctly afterwards (with its
    // real size) must still be valid, not a double-free.
    let ptr = alloc_pdf(4);
    assert!(!ptr.is_null());
    unsafe { free_pixels(ptr, 0, 100) };
    unsafe { free_pdf(ptr, 4) };
}

// ---- page_count --------------------------------------------------------------

#[test]
fn page_count_valid_pdf() {
    let n = unsafe { page_count(MINIMAL_PDF.as_ptr(), MINIMAL_PDF.len()) };
    assert_eq!(n, 1);
}

#[test]
fn page_count_garbage_bytes() {
    let bytes = b"not a pdf";
    let n = unsafe { page_count(bytes.as_ptr(), bytes.len()) };
    assert_eq!(n, -1);
}

#[test]
fn page_count_empty_buffer() {
    let empty: &[u8] = &[];
    let n = unsafe { page_count(empty.as_ptr(), 0) };
    assert_eq!(n, -1);
}

// ---- render_page ---------------------------------------------------------------

#[test]
fn render_page_happy_path_defaults() {
    let mut width_out = 0u32;
    let mut height_out = 0u32;
    let ptr = unsafe {
        render_page(
            MINIMAL_PDF.as_ptr(),
            MINIMAL_PDF.len(),
            1,
            std::ptr::null(),
            std::ptr::null(),
            &mut width_out,
            &mut height_out,
        )
    };
    assert!(!ptr.is_null());
    assert_eq!(width_out, 200);
    assert_eq!(height_out, 100);

    let len = (width_out * height_out * 4) as usize;
    let pixels = unsafe { std::slice::from_raw_parts(ptr, len) };
    assert!(
        pixels.iter().any(|&b| b != 0),
        "expected some non-blank rendered pixels"
    );

    unsafe { free_pixels(ptr, width_out, height_out) };
}

#[test]
fn render_page_width_height_override() {
    let blob = render_settings_blob(0.0, 0.0, 400, 50, [0, 0, 0, 0]);
    let mut width_out = 0u32;
    let mut height_out = 0u32;
    let ptr = unsafe {
        render_page(
            MINIMAL_PDF.as_ptr(),
            MINIMAL_PDF.len(),
            1,
            std::ptr::null(),
            blob.as_ptr(),
            &mut width_out,
            &mut height_out,
        )
    };
    assert!(!ptr.is_null());
    assert_eq!(width_out, 400);
    assert_eq!(height_out, 50);
    unsafe { free_pixels(ptr, width_out, height_out) };
}

#[test]
fn render_page_bg_color_override_shows_in_untouched_corner() {
    // Auto width/height (0/0) keeps the natural 200x100 page size. The
    // top-left pixel of the rendered image is well outside the "Hello
    // World" text (see `MINIMAL_PDF`'s doc comment), so it's guaranteed to
    // be pure background.
    let blob = render_settings_blob(0.0, 0.0, 0, 0, [10, 20, 30, 255]);
    let mut width_out = 0u32;
    let mut height_out = 0u32;
    let ptr = unsafe {
        render_page(
            MINIMAL_PDF.as_ptr(),
            MINIMAL_PDF.len(),
            1,
            std::ptr::null(),
            blob.as_ptr(),
            &mut width_out,
            &mut height_out,
        )
    };
    assert!(!ptr.is_null());
    assert_eq!(width_out, 200);
    assert_eq!(height_out, 100);

    let pixels = unsafe { std::slice::from_raw_parts(ptr, (width_out * height_out * 4) as usize) };
    assert_eq!(&pixels[0..4], &[10, 20, 30, 255]);

    unsafe { free_pixels(ptr, width_out, height_out) };
}

#[test]
fn render_page_zero_area_scale_returns_null() {
    // A tiny scale rounds the pixel dimensions down to 0x0 — confirmed
    // against a real `hayro` render while writing this test. `width_out`/
    // `height_out` must be left untouched (still their sentinel values).
    let blob = render_settings_blob(0.0001, 0.0001, 0, 0, [0, 0, 0, 0]);
    let mut width_out = 123u32;
    let mut height_out = 456u32;
    let ptr = unsafe {
        render_page(
            MINIMAL_PDF.as_ptr(),
            MINIMAL_PDF.len(),
            1,
            std::ptr::null(),
            blob.as_ptr(),
            &mut width_out,
            &mut height_out,
        )
    };
    assert!(ptr.is_null());
    assert_eq!(width_out, 123);
    assert_eq!(height_out, 456);
}

#[test]
fn render_page_zero_page_number_returns_null() {
    let mut width_out = 0u32;
    let mut height_out = 0u32;
    let ptr = unsafe {
        render_page(
            MINIMAL_PDF.as_ptr(),
            MINIMAL_PDF.len(),
            0,
            std::ptr::null(),
            std::ptr::null(),
            &mut width_out,
            &mut height_out,
        )
    };
    assert!(ptr.is_null());
}

#[test]
fn render_page_out_of_range_page_number_returns_null() {
    let mut width_out = 0u32;
    let mut height_out = 0u32;
    let ptr = unsafe {
        render_page(
            MINIMAL_PDF.as_ptr(),
            MINIMAL_PDF.len(),
            2, // MINIMAL_PDF only has 1 page
            std::ptr::null(),
            std::ptr::null(),
            &mut width_out,
            &mut height_out,
        )
    };
    assert!(ptr.is_null());
}

#[test]
fn render_page_malformed_pdf_returns_null() {
    let bytes = b"not a pdf";
    let mut width_out = 0u32;
    let mut height_out = 0u32;
    let ptr = unsafe {
        render_page(
            bytes.as_ptr(),
            bytes.len(),
            1,
            std::ptr::null(),
            std::ptr::null(),
            &mut width_out,
            &mut height_out,
        )
    };
    assert!(ptr.is_null());
}

#[test]
fn render_page_render_annotations_bytes_do_not_crash() {
    // `MINIMAL_PDF` has no annotations, so this only checks that the
    // interpreter-settings blob is decoded and plumbed through without
    // panicking/crashing, still producing a same-size image either way.
    // Actual annotation-rendering correctness is `hayro`'s own concern.
    for byte in [1u8, 2u8] {
        let blob = [byte; INTERPRETER_SETTINGS_LEN];
        let mut width_out = 0u32;
        let mut height_out = 0u32;
        let ptr = unsafe {
            render_page(
                MINIMAL_PDF.as_ptr(),
                MINIMAL_PDF.len(),
                1,
                blob.as_ptr(),
                std::ptr::null(),
                &mut width_out,
                &mut height_out,
            )
        };
        assert!(!ptr.is_null());
        assert_eq!(width_out, 200);
        assert_eq!(height_out, 100);
        unsafe { free_pixels(ptr, width_out, height_out) };
    }
}
