//! Optional compression for large, repetitive Black MIDI payloads.

use plumspirit_engine::editor::interchange::MAX_WIRE_BYTES;
use std::io::{self, Write};

const MAGIC: &[u8; 8] = b"PLUMZLIB";
const RAW: &[u8; 8] = b"PLUMCLIP";
const HEADER: usize = 16;
const INVALID: &str = "Invalid or damaged compressed musical clipboard.";
const THRESHOLD: usize = 256 * 1024;

pub(super) fn declared_size(header: &[u8], allocated: usize) -> Option<usize> {
    let magic = header.get(..8)?;
    let minimum = match magic {
        magic if magic == RAW => 33,
        magic if magic == MAGIC => HEADER + 1,
        _ => return None,
    };
    let size = u32::from_le_bytes(header.get(12..16)?.try_into().ok()?) as usize;
    (minimum..=allocated.min(MAX_WIRE_BYTES))
        .contains(&size)
        .then_some(size)
}

struct BoundedBytes {
    bytes: Vec<u8>,
    limit: usize,
}

impl Write for BoundedBytes {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(io::Error::other("compression saves too little space"));
        }
        self.bytes
            .try_reserve(buf.len())
            .map_err(io::Error::other)?;
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn pack(bytes: Vec<u8>) -> Vec<u8> {
    if bytes.len() < THRESHOLD {
        return bytes;
    }
    let mut output = BoundedBytes {
        bytes: Vec::new(),
        limit: bytes.len() * 7 / 8,
    };
    let mut header = [0; HEADER];
    header[..8].copy_from_slice(MAGIC);
    header[8..12].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
    if output.write_all(&header).is_err() {
        return bytes;
    }
    let mut encoder = flate2::write::ZlibEncoder::new(output, flate2::Compression::fast());
    if encoder.write_all(&bytes).is_err() {
        return bytes;
    }
    match encoder.finish() {
        Ok(mut output) => {
            let size = output.bytes.len() as u32;
            output.bytes[12..16].copy_from_slice(&size.to_le_bytes());
            output.bytes
        }
        Err(_) => bytes,
    }
}

pub(super) fn unpack(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    let header = bytes.get(..HEADER).ok_or(INVALID)?;
    if declared_size(header, bytes.len()) != Some(bytes.len()) {
        return Err(INVALID.into());
    }
    if &header[..8] == RAW {
        return Ok(bytes);
    }
    let size = u32::from_le_bytes(header[8..12].try_into().unwrap()) as usize;
    if !(33..=MAX_WIRE_BYTES).contains(&size) {
        return Err(INVALID.into());
    }
    let compressed = &bytes[HEADER..];
    let mut out = Vec::new();
    out.try_reserve_exact(size + 1)
        .map_err(|_| "There is not enough memory to unpack the clipboard.")?;
    out.resize(size + 1, 0);
    let mut decoder = flate2::Decompress::new(true);
    let status = decoder
        .decompress(compressed, &mut out, flate2::FlushDecompress::Finish)
        .map_err(|_| INVALID)?;
    if status != flate2::Status::StreamEnd
        || decoder.total_out() != size as u64
        || decoder.total_in() != compressed.len() as u64
    {
        return Err(INVALID.into());
    }
    out.truncate(size);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compressed_wrapper_rejects_size_mismatch_and_extra_data() {
        let size = 512 * 1024;
        let mut data = vec![2; size];
        data[..8].copy_from_slice(RAW);
        data[12..16].copy_from_slice(&(size as u32).to_le_bytes());
        let packed = pack(data.clone());
        assert!(packed.len() < size / 10);
        assert_eq!(unpack(packed.clone()).unwrap(), data);
        for len in [33, (MAX_WIRE_BYTES + 1) as u32, size as u32 + 1] {
            let mut bad = packed.clone();
            bad[8..12].copy_from_slice(&len.to_le_bytes());
            assert!(unpack(bad).is_err());
        }
        let mut bad = packed;
        bad.push(0);
        let size = bad.len() as u32;
        bad[12..16].copy_from_slice(&size.to_le_bytes());
        assert!(unpack(bad).is_err());
    }
}
