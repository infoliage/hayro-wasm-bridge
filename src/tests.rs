//!
//! Copyright 2026 Infoliage LLC. All Rights Reserved.
//! Use is subject to license terms.
//!
//! SPDX-License-Identifier: Apache-2.0 OR MIT
//!
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

/// Same page geometry as `MINIMAL_PDF` (a 200x100pt page), but with a
/// `/Rotate 90` entry and a full document information dictionary — for
/// exercising [`page_info`]'s rotation handling and [`document_info`]'s
/// metadata/date decoding. `CreationDate` carries an explicit `+05'30'`
/// offset, `ModDate` a bare `Z` (both should round-trip through
/// [`date_str`], `Z` folding to `+00:00` same as no offset at all).
const PDF_ROTATED_WITH_METADATA: &[u8] = b"%PDF-1.5
1 0 obj
<< /Type /Catalog /Pages 2 0 R >>
endobj
2 0 obj
<< /Type /Pages /Kids [3 0 R] /Count 1 >>
endobj
3 0 obj
<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Rotate 90 /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>
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
6 0 obj
<< /Title (My Title) /Author (Jane Doe) /Subject (A Subject) /Keywords (foo bar) /Creator (My Creator) /Producer (My Producer) /CreationDate (D:20200102030405+05'30') /ModDate (D:20210607080910Z) >>
endobj
xref
0 7
0000000000 65535 f
trailer
<< /Size 7 /Root 1 0 R /Info 6 0 R >>
startxref
0
%%EOF";

// ---- Render-settings blob decoding -----------------------------------------

#[test]
fn render_settings_null_ptr_is_default() {
    let settings = unsafe { read_render_settings(std::ptr::null(), 0) }.unwrap();
    let defaults = RenderSettings::default();
    assert_eq!(settings.x_scale, defaults.x_scale);
    assert_eq!(settings.y_scale, defaults.y_scale);
    assert_eq!(settings.width, defaults.width);
    assert_eq!(settings.height, defaults.height);
    assert_eq!(settings.bg_color.to_rgba8(), defaults.bg_color.to_rgba8());
}

#[test]
fn render_settings_empty_object_is_default() {
    // `{}` is the JSON equivalent of the old zeroed-blob convention: every
    // field absent, so every field falls back to `hayro`'s default.
    let json = b"{}";
    let settings = unsafe { read_render_settings(json.as_ptr(), json.len()) }.unwrap();
    let defaults = RenderSettings::default();
    assert_eq!(settings.x_scale, defaults.x_scale);
    assert_eq!(settings.y_scale, defaults.y_scale);
    assert_eq!(settings.width, defaults.width);
    assert_eq!(settings.height, defaults.height);
    assert_eq!(settings.bg_color.to_rgba8(), defaults.bg_color.to_rgba8());
}

#[test]
fn render_settings_decodes_explicit_values() {
    let json = br#"{"x_scale":2.5,"y_scale":3.5,"width":800,"height":600,"bg_color":{"r":10,"g":20,"b":30,"a":128}}"#;
    let settings = unsafe { read_render_settings(json.as_ptr(), json.len()) }.unwrap();
    assert_eq!(settings.x_scale, 2.5);
    assert_eq!(settings.y_scale, 3.5);
    assert_eq!(settings.width, Some(800));
    assert_eq!(settings.height, Some(600));
    assert_eq!(
        settings.bg_color.to_rgba8(),
        AlphaColor::from_rgba8(10, 20, 30, 128).to_rgba8()
    );
}

#[test]
fn render_settings_partial_object_only_overrides_what_is_set() {
    let json = br#"{"width":800}"#;
    let settings = unsafe { read_render_settings(json.as_ptr(), json.len()) }.unwrap();
    let defaults = RenderSettings::default();
    assert_eq!(settings.width, Some(800));
    assert_eq!(settings.height, defaults.height);
    assert_eq!(settings.x_scale, defaults.x_scale);
}

#[test]
fn render_settings_explicit_zero_scale_is_not_default() {
    // The whole point of moving to real JSON `option`s instead of the old
    // "0 means default" byte convention: an explicit 0.0 is honored
    // literally now, not silently reinterpreted.
    let json = br#"{"x_scale":0.0}"#;
    let settings = unsafe { read_render_settings(json.as_ptr(), json.len()) }.unwrap();
    assert_eq!(settings.x_scale, 0.0);
}

#[test]
fn render_settings_malformed_json_is_an_error() {
    let json = b"not json";
    assert!(unsafe { read_render_settings(json.as_ptr(), json.len()) }.is_err());
}

#[test]
fn render_settings_unknown_field_is_an_error() {
    // Catches typos/drift between the host and this module's field names,
    // rather than silently ignoring an unrecognized field.
    let json = br#"{"xscale":2.5}"#;
    assert!(unsafe { read_render_settings(json.as_ptr(), json.len()) }.is_err());
}

#[test]
fn render_settings_wrong_field_type_is_an_error() {
    let json = br#"{"x_scale":"not a number"}"#;
    assert!(unsafe { read_render_settings(json.as_ptr(), json.len()) }.is_err());
}

#[test]
fn render_settings_out_of_range_number_is_an_error() {
    // `width` is `u16`; 70000 overflows it.
    let json = br#"{"width":70000}"#;
    assert!(unsafe { read_render_settings(json.as_ptr(), json.len()) }.is_err());

    // `bg_color.r` is `u8`; 300 overflows it.
    let json = br#"{"bg_color":{"r":300,"g":0,"b":0,"a":0}}"#;
    assert!(unsafe { read_render_settings(json.as_ptr(), json.len()) }.is_err());
}

#[test]
fn render_settings_wrong_top_level_shape_is_an_error() {
    // Valid JSON, but not an object - an array, a bare string, a bare
    // number, and `null` should all be rejected the same as syntactically
    // invalid JSON, not e.g. silently treated as "no fields set".
    for json in [b"[1,2,3]".as_slice(), b"\"oops\"", b"42", b"null"] {
        assert!(
            unsafe { read_render_settings(json.as_ptr(), json.len()) }.is_err(),
            "expected {:?} to be rejected",
            std::str::from_utf8(json).unwrap()
        );
    }
}

#[test]
fn render_settings_empty_body_with_nonnull_ptr_is_an_error() {
    // Distinct from a null pointer (which means "use every default"): a
    // non-null pointer with a zero-length, empty JSON body isn't valid
    // JSON at all, so it must be rejected rather than treated the same as
    // "no settings supplied".
    let empty: &[u8] = &[];
    assert!(unsafe { read_render_settings(empty.as_ptr(), 0) }.is_err());
}

// ---- Interpreter-settings blob decoding ------------------------------------

#[test]
fn interpreter_settings_null_ptr_is_default() {
    let settings = unsafe { read_interpreter_settings(std::ptr::null(), 0) }.unwrap();
    assert_eq!(
        settings.render_annotations,
        InterpreterSettings::default().render_annotations
    );
}

#[test]
fn interpreter_settings_empty_object_is_default() {
    let json = b"{}";
    let settings = unsafe { read_interpreter_settings(json.as_ptr(), json.len()) }.unwrap();
    assert_eq!(
        settings.render_annotations,
        InterpreterSettings::default().render_annotations
    );
}

#[test]
fn interpreter_settings_true_enables_annotations() {
    let json = br#"{"render_annotations":true}"#;
    let settings = unsafe { read_interpreter_settings(json.as_ptr(), json.len()) }.unwrap();
    assert!(settings.render_annotations);
}

#[test]
fn interpreter_settings_false_disables_annotations() {
    let json = br#"{"render_annotations":false}"#;
    let settings = unsafe { read_interpreter_settings(json.as_ptr(), json.len()) }.unwrap();
    assert!(!settings.render_annotations);
}

#[test]
fn interpreter_settings_malformed_json_is_an_error() {
    let json = b"{";
    assert!(unsafe { read_interpreter_settings(json.as_ptr(), json.len()) }.is_err());
}

#[test]
fn interpreter_settings_wrong_field_type_is_an_error() {
    let json = br#"{"render_annotations":"yes"}"#;
    assert!(unsafe { read_interpreter_settings(json.as_ptr(), json.len()) }.is_err());
}

#[test]
fn interpreter_settings_unknown_field_is_an_error() {
    let json = br#"{"renderAnnotations":true}"#;
    assert!(unsafe { read_interpreter_settings(json.as_ptr(), json.len()) }.is_err());
}

#[test]
fn interpreter_settings_wrong_top_level_shape_is_an_error() {
    // `[true]` is deliberately not in this list: serde's derived
    // `Deserialize` for a struct accepts a JSON array positionally (field
    // order = declaration order) as well as an object, and
    // `InterpreterSettingsJson` has exactly one field, so a single-element
    // array is legitimately equivalent to `{"render_annotations": true}`
    // as far as serde is concerned - not a bug, just worth knowing about
    // for a one-field settings struct specifically. `RenderSettingsJson`
    // has five fields, so this doesn't come up for it (see the
    // `render_settings` equivalent of this test).
    for json in [b"[true, false]".as_slice(), b"\"oops\"", b"1", b"null"] {
        assert!(
            unsafe { read_interpreter_settings(json.as_ptr(), json.len()) }.is_err(),
            "expected {:?} to be rejected",
            std::str::from_utf8(json).unwrap()
        );
    }
}

#[test]
fn interpreter_settings_empty_body_with_nonnull_ptr_is_an_error() {
    let empty: &[u8] = &[];
    assert!(unsafe { read_interpreter_settings(empty.as_ptr(), 0) }.is_err());
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
fn alloc_render_settings_round_trip() {
    let ptr = alloc_render_settings(2);
    assert!(!ptr.is_null());
    unsafe { free_render_settings(ptr, 2) };
}

#[test]
fn alloc_render_settings_zero_size_returns_null() {
    assert!(alloc_render_settings(0).is_null());
}

#[test]
fn free_render_settings_null_is_noop() {
    unsafe { free_render_settings(std::ptr::null_mut(), 0) };
    unsafe { free_render_settings(std::ptr::null_mut(), 2) };
}

#[test]
fn alloc_interpreter_settings_round_trip() {
    let ptr = alloc_interpreter_settings(2);
    assert!(!ptr.is_null());
    unsafe { free_interpreter_settings(ptr, 2) };
}

#[test]
fn free_interpreter_settings_null_is_noop() {
    unsafe { free_interpreter_settings(std::ptr::null_mut(), 0) };
    unsafe { free_interpreter_settings(std::ptr::null_mut(), 2) };
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

#[test]
fn pixel_byte_len_normal_case() {
    assert_eq!(pixel_byte_len(200, 100), Some(200 * 100 * 4));
}

#[test]
fn pixel_byte_len_overflow_is_none() {
    // u32::MAX * u32::MAX * 4 overflows even a u64 intermediate, not just
    // usize - this pins down the actual return value, not just "didn't
    // panic".
    assert_eq!(pixel_byte_len(u32::MAX, u32::MAX), None);
}

#[test]
fn free_pixels_overflowing_size_is_a_noop_not_ub() {
    // Same overflow as above, but through the public free_pixels export:
    // must decline to free rather than calling `dealloc` with a wrapped
    // (and wrong) size, which would be undefined behavior.
    let ptr = alloc_pdf(4);
    assert!(!ptr.is_null());
    unsafe { free_pixels(ptr, u32::MAX, u32::MAX) };
    unsafe { free_pdf(ptr, 4) };
}

// ---- render_page --------------------------------------------------------------

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
            0,
            std::ptr::null(),
            0,
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
    let json = br#"{"width":400,"height":50}"#;
    let mut width_out = 0u32;
    let mut height_out = 0u32;
    let ptr = unsafe {
        render_page(
            MINIMAL_PDF.as_ptr(),
            MINIMAL_PDF.len(),
            1,
            std::ptr::null(),
            0,
            json.as_ptr(),
            json.len(),
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
    // The top-left pixel of the rendered image is well outside the "Hello
    // World" text (see `MINIMAL_PDF`'s doc comment), so it's guaranteed to
    // be pure background.
    let json = br#"{"bg_color":{"r":10,"g":20,"b":30,"a":255}}"#;
    let mut width_out = 0u32;
    let mut height_out = 0u32;
    let ptr = unsafe {
        render_page(
            MINIMAL_PDF.as_ptr(),
            MINIMAL_PDF.len(),
            1,
            std::ptr::null(),
            0,
            json.as_ptr(),
            json.len(),
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
    let json = br#"{"x_scale":0.0001,"y_scale":0.0001}"#;
    let mut width_out = 123u32;
    let mut height_out = 456u32;
    let ptr = unsafe {
        render_page(
            MINIMAL_PDF.as_ptr(),
            MINIMAL_PDF.len(),
            1,
            std::ptr::null(),
            0,
            json.as_ptr(),
            json.len(),
            &mut width_out,
            &mut height_out,
        )
    };
    assert!(ptr.is_null());
    assert_eq!(width_out, 123);
    assert_eq!(height_out, 456);
}

#[test]
fn render_page_explicit_zero_scale_is_zero_area_not_default() {
    // Confirms the semantic change end to end: `"x_scale":0.0` must *not*
    // be reinterpreted as "use the default" the way the old byte layout's
    // `0.0` sentinel was.
    let json = br#"{"x_scale":0.0,"y_scale":0.0}"#;
    let mut width_out = 0u32;
    let mut height_out = 0u32;
    let ptr = unsafe {
        render_page(
            MINIMAL_PDF.as_ptr(),
            MINIMAL_PDF.len(),
            1,
            std::ptr::null(),
            0,
            json.as_ptr(),
            json.len(),
            &mut width_out,
            &mut height_out,
        )
    };
    assert!(ptr.is_null());
}

#[test]
fn render_page_out_of_range_page_returns_null() {
    let mut width_out = 0u32;
    let mut height_out = 0u32;
    let ptr = unsafe {
        render_page(
            MINIMAL_PDF.as_ptr(),
            MINIMAL_PDF.len(),
            2,
            std::ptr::null(),
            0,
            std::ptr::null(),
            0,
            &mut width_out,
            &mut height_out,
        )
    };
    assert!(ptr.is_null());
}

#[test]
fn render_page_malformed_render_settings_json_returns_null() {
    let json = b"not json";
    let mut width_out = 0u32;
    let mut height_out = 0u32;
    let ptr = unsafe {
        render_page(
            MINIMAL_PDF.as_ptr(),
            MINIMAL_PDF.len(),
            1,
            std::ptr::null(),
            0,
            json.as_ptr(),
            json.len(),
            &mut width_out,
            &mut height_out,
        )
    };
    assert!(ptr.is_null());
}

#[test]
fn render_page_malformed_interpreter_settings_json_returns_null() {
    let json = b"not json";
    let mut width_out = 0u32;
    let mut height_out = 0u32;
    let ptr = unsafe {
        render_page(
            MINIMAL_PDF.as_ptr(),
            MINIMAL_PDF.len(),
            1,
            json.as_ptr(),
            json.len(),
            std::ptr::null(),
            0,
            &mut width_out,
            &mut height_out,
        )
    };
    assert!(ptr.is_null());
}

#[test]
fn render_page_render_annotations_does_not_crash() {
    // `MINIMAL_PDF` has no annotations, so this only checks that the
    // interpreter-settings blob is decoded and plumbed through without
    // panicking/crashing, still producing a same-size image either way.
    // Actual annotation-rendering correctness is `hayro`'s own concern.
    let payloads: [&[u8]; 2] = [
        br#"{"render_annotations":true}"#,
        br#"{"render_annotations":false}"#,
    ];
    for json in payloads {
        let mut width_out = 0u32;
        let mut height_out = 0u32;
        let ptr = unsafe {
            render_page(
                MINIMAL_PDF.as_ptr(),
                MINIMAL_PDF.len(),
                1,
                json.as_ptr(),
                json.len(),
                std::ptr::null(),
                0,
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

// ---- page_info ----------------------------------------------------------

/// Parse a blob returned by [`page_info`]/[`document_info`] back into a
/// `serde_json::Value`, for asserting against by field.
unsafe fn parse_json_out(ptr: *mut u8, len: u32) -> serde_json::Value {
    // SAFETY: the caller upholds this function's safety contract (`ptr`
    // must describe `len` live, initialized bytes).
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    serde_json::from_slice(bytes).expect("page_info/document_info should emit valid JSON")
}

#[test]
fn page_info_happy_path_defaults() {
    let mut len_out = 0u32;
    let ptr = unsafe { page_info(MINIMAL_PDF.as_ptr(), MINIMAL_PDF.len(), 1, &mut len_out) };
    assert!(!ptr.is_null());

    let json = unsafe { parse_json_out(ptr, len_out) };
    assert_eq!(json["width"], 200.0);
    assert_eq!(json["height"], 100.0);
    assert_eq!(json["rotation"], 0);
    assert_eq!(
        json["media_box"],
        serde_json::json!({"x0": 0.0, "y0": 0.0, "x1": 200.0, "y1": 100.0})
    );
    assert_eq!(json["crop_box"], json["media_box"]);

    unsafe { free_page_info(ptr, len_out) };
}

#[test]
fn page_info_rotation_swaps_render_dimensions_but_not_boxes() {
    let mut len_out = 0u32;
    let ptr = unsafe {
        page_info(
            PDF_ROTATED_WITH_METADATA.as_ptr(),
            PDF_ROTATED_WITH_METADATA.len(),
            1,
            &mut len_out,
        )
    };
    assert!(!ptr.is_null());

    let json = unsafe { parse_json_out(ptr, len_out) };
    assert_eq!(json["rotation"], 90);
    // `width`/`height` (render_dimensions) are swapped for a 90° rotation...
    assert_eq!(json["width"], 100.0);
    assert_eq!(json["height"], 200.0);
    // ...but the raw media/crop boxes are not.
    assert_eq!(
        json["media_box"],
        serde_json::json!({"x0": 0.0, "y0": 0.0, "x1": 200.0, "y1": 100.0})
    );

    unsafe { free_page_info(ptr, len_out) };
}

#[test]
fn page_info_page_number_zero_returns_null() {
    let mut len_out = 0u32;
    let ptr = unsafe { page_info(MINIMAL_PDF.as_ptr(), MINIMAL_PDF.len(), 0, &mut len_out) };
    assert!(ptr.is_null());
}

#[test]
fn page_info_out_of_range_page_returns_null() {
    let mut len_out = 0u32;
    let ptr = unsafe { page_info(MINIMAL_PDF.as_ptr(), MINIMAL_PDF.len(), 2, &mut len_out) };
    assert!(ptr.is_null());
}

#[test]
fn page_info_garbage_pdf_returns_null() {
    let bytes = b"not a pdf";
    let mut len_out = 0u32;
    let ptr = unsafe { page_info(bytes.as_ptr(), bytes.len(), 1, &mut len_out) };
    assert!(ptr.is_null());
}

#[test]
fn free_page_info_null_is_noop() {
    unsafe { free_page_info(std::ptr::null_mut(), 0) };
}

// ---- document_info --------------------------------------------------------

#[test]
fn document_info_no_info_dict_is_all_null_but_page_count_and_version_are_set() {
    let mut len_out = 0u32;
    let ptr = unsafe { document_info(MINIMAL_PDF.as_ptr(), MINIMAL_PDF.len(), &mut len_out) };
    assert!(!ptr.is_null());

    let json = unsafe { parse_json_out(ptr, len_out) };
    assert_eq!(json["page_count"], 1);
    assert_eq!(json["version"], "1.1");
    for field in [
        "title",
        "author",
        "subject",
        "keywords",
        "creator",
        "producer",
        "creation_date",
        "modification_date",
    ] {
        assert!(json[field].is_null(), "expected {field} to be null");
    }

    unsafe { free_document_info(ptr, len_out) };
}

#[test]
fn document_info_decodes_metadata_and_dates() {
    let mut len_out = 0u32;
    let ptr = unsafe {
        document_info(
            PDF_ROTATED_WITH_METADATA.as_ptr(),
            PDF_ROTATED_WITH_METADATA.len(),
            &mut len_out,
        )
    };
    assert!(!ptr.is_null());

    let json = unsafe { parse_json_out(ptr, len_out) };
    assert_eq!(json["page_count"], 1);
    assert_eq!(json["version"], "1.5");
    assert_eq!(json["title"], "My Title");
    assert_eq!(json["author"], "Jane Doe");
    assert_eq!(json["subject"], "A Subject");
    assert_eq!(json["keywords"], "foo bar");
    assert_eq!(json["creator"], "My Creator");
    assert_eq!(json["producer"], "My Producer");
    assert_eq!(json["creation_date"], "2020-01-02T03:04:05+05:30");
    // `ModDate`'s bare `Z` and "no offset suffix at all" both parse to the
    // same zeroed-offset representation upstream, so this round-trips as
    // `+00:00`, not `Z`.
    assert_eq!(json["modification_date"], "2021-06-07T08:09:10+00:00");

    unsafe { free_document_info(ptr, len_out) };
}

#[test]
fn document_info_garbage_pdf_returns_null() {
    let bytes = b"not a pdf";
    let mut len_out = 0u32;
    let ptr = unsafe { document_info(bytes.as_ptr(), bytes.len(), &mut len_out) };
    assert!(ptr.is_null());
}

#[test]
fn document_info_empty_buffer_returns_null() {
    let empty: &[u8] = &[];
    let mut len_out = 0u32;
    let ptr = unsafe { document_info(empty.as_ptr(), 0, &mut len_out) };
    assert!(ptr.is_null());
}

#[test]
fn free_document_info_null_is_noop() {
    unsafe { free_document_info(std::ptr::null_mut(), 0) };
}
