//! WebAssembly bindings for `fast-grapheme-segmenter`, as a plain C ABI over
//! linear memory — no `wasm-bindgen`, no toolchain beyond `rustc`.
//!
//! The core crate stays `forbid(unsafe_code)`; this glue is the only place
//! raw pointers exist, and every contract is spelled out at the export.
//!
//! JS callers store strings as UTF-16 and want UTF-16 offsets back, so the
//! `*_utf16` entry points take and return code units — no transcoding on
//! either side of the boundary. The `*_utf8` entry points serve embedders
//! that already hold UTF-8.
//!
//! All buffers come from [`fgs_alloc`], which hands out 8-byte-aligned
//! pointers, so the `u16`/`u32` views over them are well-aligned. Writing a
//! buffer from JS after any call that might grow memory must re-create the
//! `TypedArray` views, since growth detaches the old `ArrayBuffer`; the
//! wrapper in `grapheme-segmenter.mjs` does this.

use fast_grapheme_segmenter::{
    UNICODE_VERSION, count_graphemes, count_graphemes_utf16, grapheme_boundaries,
    grapheme_boundaries_utf16,
};
use std::alloc::{Layout, alloc, dealloc};

/// Alignment of every buffer [`fgs_alloc`] returns.
const ALIGN: usize = 8;

/// The Unicode version the segmenter tables encode, packed as
/// `major << 16 | minor << 8 | patch`.
#[unsafe(no_mangle)]
pub extern "C" fn fgs_unicode_version() -> u32 {
    (UNICODE_VERSION.0 as u32) << 16 | (UNICODE_VERSION.1 as u32) << 8 | UNICODE_VERSION.2 as u32
}

/// Allocate `len` bytes of wasm linear memory, 8-byte aligned.
///
/// The pointer must be released by [`fgs_free`] with the same `len`. May
/// grow memory, which detaches existing JS views over the old buffer.
///
/// # Safety
///
/// `len > 0`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fgs_alloc(len: usize) -> *mut u8 {
    // Edition 2024 keeps `unsafe extern` fn bodies safe contexts, so the
    // raw operations carry their own blocks.
    unsafe { alloc(Layout::from_size_align(len, ALIGN).unwrap()) }
}

/// Release a buffer from [`fgs_alloc`], passing the same `len`.
///
/// # Safety
///
/// `ptr` must come from [`fgs_alloc`], with the same `len`, and not have
/// been released already.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fgs_free(ptr: *mut u8, len: usize) {
    unsafe { dealloc(ptr, Layout::from_size_align(len, ALIGN).unwrap()) }
}

/// Count the extended grapheme clusters of a UTF-8 string.
///
/// Returns `usize::MAX` when the input is not well-formed UTF-8.
///
/// # Safety
///
/// `ptr` must point to `len` readable bytes for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fgs_count_utf8(ptr: *const u8, len: usize) -> usize {
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    match std::str::from_utf8(bytes) {
        Ok(s) => count_graphemes(s),
        Err(_) => usize::MAX,
    }
}

/// Count the extended grapheme clusters of a UTF-16 slice, `len` being the
/// number of code units. Unpaired surrogates count as one cluster each.
///
/// # Safety
///
/// `ptr` must point to `len` readable `u16`s for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fgs_count_utf16(ptr: *const u16, len: usize) -> usize {
    count_graphemes_utf16(unsafe { std::slice::from_raw_parts(ptr, len) })
}

/// Write the UTF-8 byte offsets at which clusters start into `out`, and
/// return the total number of boundaries.
///
/// At most `out_len` offsets are written; a smaller return than the buffer
/// allows never happens, and a return larger than `out_len` means the caller
/// should re-call with a bigger buffer. The input must be well-formed UTF-8,
/// which the return of `usize::MAX` signals otherwise.
///
/// # Safety
///
/// `ptr` must point to `len` readable bytes and `out` to `out_len` writable
/// `u32`s, for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fgs_boundaries_utf8(
    ptr: *const u8,
    len: usize,
    out: *mut u32,
    out_len: usize,
) -> usize {
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    let s = match std::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(_) => return usize::MAX,
    };
    let mut total = 0;
    for offset in grapheme_boundaries(s) {
        if total < out_len {
            unsafe { *out.add(total) = offset as u32 };
        }
        total += 1;
    }
    total
}

/// Write the UTF-16 code unit offsets at which clusters start into `out`,
/// and return the total number of boundaries — the entry point for JS,
/// whose string indices are UTF-16 code units.
///
/// The contract is that of [`fgs_boundaries_utf8`], with units in place of
/// bytes. Unpaired surrogates are their own cluster and never glue.
///
/// # Safety
///
/// `ptr` must point to `len` readable `u16`s and `out` to `out_len`
/// writable `u32`s, for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fgs_boundaries_utf16(
    ptr: *const u16,
    len: usize,
    out: *mut u32,
    out_len: usize,
) -> usize {
    let units = unsafe { std::slice::from_raw_parts(ptr, len) };
    let mut total = 0;
    for offset in grapheme_boundaries_utf16(units) {
        if total < out_len {
            unsafe { *out.add(total) = offset as u32 };
        }
        total += 1;
    }
    total
}

/// The `wasm-bindgen` surface, enabled with `--features bindgen`.
///
/// Where the raw C ABI hands JS UTF-16 entry points so nothing transcodes,
/// these bindings take `&str` and let wasm-bindgen marshal — comfortable,
/// typed, and one UTF-16→UTF-8 conversion per call, which is the trade the
/// two layers offer. Offsets still come back as UTF-16 code units so they
/// index the original JS string.
#[cfg(feature = "bindgen")]
mod bindgen {
    use fast_grapheme_segmenter::{
        UNICODE_VERSION, count_graphemes, count_graphemes_utf16, grapheme_boundaries,
        grapheme_boundaries_utf16,
    };
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen]
    pub fn unicode_version() -> String {
        format!(
            "{}.{}.{}",
            UNICODE_VERSION.0, UNICODE_VERSION.1, UNICODE_VERSION.2
        )
    }

    /// Count the extended grapheme clusters of a string.
    #[wasm_bindgen]
    pub fn count(s: &str) -> usize {
        count_graphemes(s)
    }

    /// Count the extended grapheme clusters of UTF-16 code units, skipping
    /// wasm-bindgen's string marshaling.
    #[wasm_bindgen]
    pub fn count_utf16(units: &[u16]) -> usize {
        count_graphemes_utf16(units)
    }

    /// UTF-16 code unit offsets at which each cluster starts, indexing the
    /// original JS string.
    #[wasm_bindgen]
    pub fn boundaries(s: &str) -> Vec<u32> {
        let mut out = Vec::new();
        let mut bounds = grapheme_boundaries(s);
        let mut target = bounds.next();
        let mut units = 0u32;
        for (i, ch) in s.char_indices() {
            if target == Some(i) {
                out.push(units);
                target = bounds.next();
            }
            units += ch.len_utf16() as u32;
        }
        out
    }

    /// [`boundaries`] over UTF-16 code units, no marshaling.
    #[wasm_bindgen]
    pub fn boundaries_utf16(units: &[u16]) -> Vec<u32> {
        grapheme_boundaries_utf16(units)
            .map(|offset| offset as u32)
            .collect()
    }
}
