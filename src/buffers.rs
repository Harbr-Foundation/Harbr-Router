//! High-performance buffer management for zero-copy operations.

use bytes::BytesMut;
use once_cell::sync::Lazy;

/// Buffer size classes for different use cases
const BUFFER_SIZES: &[usize] = &[1024, 4096, 8192, 16384, 32768, 65536];

/// Global buffer pool instance
static GLOBAL_BUFFER_POOL: Lazy<()> = Lazy::new(|| ());

/// Get a buffer with the specified minimum size.
pub fn get_buffer(min_size: usize) -> BytesMut {
    let size = BUFFER_SIZES.iter()
        .find(|&&size| size >= min_size)
        .copied()
        .unwrap_or_else(|| min_size.next_power_of_two().max(4096));
    
    BytesMut::with_capacity(size)
}

/// Get a small buffer (4KB) - optimized for common case.
pub fn get_small_buffer() -> BytesMut {
    get_buffer(4096)
}

/// Get a medium buffer (16KB) - good for most transfers.
pub fn get_medium_buffer() -> BytesMut {
    get_buffer(16384)
}

/// Get a large buffer (64KB) - for high-throughput scenarios.
pub fn get_large_buffer() -> BytesMut {
    get_buffer(65536)
}