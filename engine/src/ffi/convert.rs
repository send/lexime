use std::ffi::{c_char, CString};
use std::ptr;

use super::OwnedVec;
use crate::converter;

// --- Conversion types (used by Candidate Response) ---

#[repr(C)]
pub struct LexSegment {
    pub reading: *const c_char,
    pub surface: *const c_char,
}

#[repr(C)]
pub struct LexConversionResult {
    pub segments: *const LexSegment,
    pub len: u32,
    pub(crate) _owned: *mut OwnedVec<LexSegment>,
}

impl LexConversionResult {
    pub(crate) fn empty() -> Self {
        Self {
            segments: ptr::null(),
            len: 0,
            _owned: ptr::null_mut(),
        }
    }
}

/// Pack a list of ConvertedSegments into a C-compatible LexConversionResult.
pub(crate) fn pack_conversion_result(
    result: Vec<converter::ConvertedSegment>,
) -> LexConversionResult {
    let mut strings = Vec::with_capacity(result.len() * 2);
    let mut segments = Vec::with_capacity(result.len());

    for seg in &result {
        let Ok(reading) = CString::new(seg.reading.as_str()) else {
            continue;
        };
        let Ok(surface) = CString::new(seg.surface.as_str()) else {
            continue;
        };
        segments.push(LexSegment {
            reading: reading.as_ptr(),
            surface: surface.as_ptr(),
        });
        strings.push(reading);
        strings.push(surface);
    }

    let (ptr, len, owned) = OwnedVec::pack(segments, strings);
    if owned.is_null() {
        return LexConversionResult::empty();
    }
    LexConversionResult {
        segments: ptr,
        len,
        _owned: owned,
    }
}
