use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
};

pub fn u128_to_u64_safe(x: u128) -> Option<u64> {
    if x <= u64::MAX as u128 {
        Some(x as u64)
    } else {
        None
    }
}

pub fn read_bytes_at(file: &mut File, start: u128, len: usize) -> io::Result<Vec<u8>> {
    let offset = match u128_to_u64_safe(start) {
        Some(off) => off,
        None => return Ok(vec![]),
    };
    file.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0u8; len];
    let n = file.read(&mut buf)?;
    buf.truncate(n);
    Ok(buf)
}