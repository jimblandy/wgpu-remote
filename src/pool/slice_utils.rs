//! Utilities for working with `*mut[u8]`.

use std::ops::Range;

pub const fn ptr_subslice(ptr_slice: *mut [u8], range: Range<usize>) -> *mut [u8] {
    assert!(range.start < range.end);
    assert!(range.end <= ptr_slice.len());
    std::ptr::slice_from_raw_parts_mut(
        (ptr_slice as *mut u8).wrapping_add(range.start),
        range.end - range.start,
    )
}

#[test]
fn subslice_of_uninit() {
    use std::mem;
    let mut bytes = mem::MaybeUninit::<[u8; 10]>::uninit();
    let full_slice: *mut [u8] = bytes.as_mut_ptr();
    let subrange = ptr_subslice(full_slice, 2..8);
    assert_eq!(subrange.len(), 6);
}

pub fn ptr_subslice_range(outer: *mut [u8], inner: *mut [u8]) -> Range<usize> {
    let outer_start = outer as *mut u8;
    let inner_start = inner as *mut u8;
    assert!(outer_start <= inner_start);
    let start_offset = inner_start as usize - outer_start as usize;
    let end_offset = start_offset + inner.len();
    assert!(end_offset <= outer.len());
    start_offset..end_offset
}

#[test]
fn subslice_range_of_uninit() {
    let mut bytes = [0_u8; 10];
    let full_slice: *mut [u8] = &mut bytes;
    let partial_slice: *mut [u8] = &mut bytes[2..8];
    assert_eq!(ptr_subslice_range(full_slice, partial_slice), 2..8);
}
