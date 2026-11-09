//! Tiny uncompressed grayscale BigTIFF fixture, using inline single-value tags.
//! Exercises the existing TIFF decoder's basename-hint fallback without adding
//! another codec dependency to the application or test build.
pub fn grayscale_pixel() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"II");
    bytes.extend_from_slice(&43u16.to_le_bytes());
    bytes.extend_from_slice(&8u16.to_le_bytes());
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes.extend_from_slice(&16u64.to_le_bytes());
    let entries: [(u16, u16, u64); 9] = [
        (256, 4, 1),    // width, LONG
        (257, 4, 1),    // height, LONG
        (258, 3, 8),    // bits/sample, SHORT
        (259, 3, 1),    // no compression
        (262, 3, 1),    // black-is-zero grayscale
        (273, 16, 212), // strip offset, LONG8
        (277, 3, 1),    // one sample/pixel
        (278, 4, 1),    // one row/strip
        (279, 16, 1),   // one byte/strip, LONG8
    ];
    bytes.extend_from_slice(&(entries.len() as u64).to_le_bytes());
    for (tag, kind, value) in entries {
        bytes.extend_from_slice(&tag.to_le_bytes());
        bytes.extend_from_slice(&kind.to_le_bytes());
        bytes.extend_from_slice(&1u64.to_le_bytes());
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes.extend_from_slice(&0u64.to_le_bytes());
    assert_eq!(bytes.len(), 212);
    bytes.push(77);
    bytes
}

/// Header-only large RGBA32F image: resource preflight must reject before
/// allocating/attempting to decode its deliberately absent strip payload.
pub fn rgba_float_header(width: u32, height: u32) -> Vec<u8> {
    let entries: [(u16, u16, u64, u64); 11] = [
        (256, 4, 1, u64::from(width)),
        (257, 4, 1, u64::from(height)),
        (258, 3, 4, 0x0020_0020_0020_0020),
        (259, 3, 1, 1),
        (262, 3, 1, 2),
        (273, 16, 1, 252),
        (277, 3, 1, 4),
        (278, 4, 1, u64::from(height)),
        (279, 16, 1, u64::from(width) * u64::from(height) * 16),
        (338, 3, 1, 2),
        (339, 3, 4, 0x0003_0003_0003_0003),
    ];
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"II");
    bytes.extend_from_slice(&43u16.to_le_bytes());
    bytes.extend_from_slice(&8u16.to_le_bytes());
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes.extend_from_slice(&16u64.to_le_bytes());
    bytes.extend_from_slice(&(entries.len() as u64).to_le_bytes());
    for (tag, kind, count, value) in entries {
        bytes.extend_from_slice(&tag.to_le_bytes());
        bytes.extend_from_slice(&kind.to_le_bytes());
        bytes.extend_from_slice(&count.to_le_bytes());
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes.extend_from_slice(&0u64.to_le_bytes());
    assert_eq!(bytes.len(), 252);
    bytes
}
