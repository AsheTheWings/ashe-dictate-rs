use std::ffi::{c_int, c_void};

/// Supplies the C23 secure-wipe symbol referenced by the current precompiled
/// Libsodium MinGW archive but absent from the Windows CRT.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memset_explicit(
    destination: *mut c_void,
    value: c_int,
    length: usize,
) -> *mut c_void {
    let cursor = destination.cast::<u8>();
    for offset in 0..length {
        // SAFETY: this function has the same caller contract as C memset.
        unsafe { cursor.add(offset).write_volatile(value as u8) };
    }
    destination
}
