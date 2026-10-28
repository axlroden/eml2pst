//! CRC-32 as used by MS-PST section 5.3: the reflected 0xEDB88320 polynomial,
//! seeded with 0 and without the final inversion that zlib applies.

const TABLE: [u32; 256] = make_table();

const fn make_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

pub fn crc32(data: &[u8]) -> u32 {
    data.iter().fold(0u32, |crc, &b| {
        TABLE[((crc ^ u32::from(b)) & 0xFF) as usize] ^ (crc >> 8)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zeros_hash_to_zero() {
        // A zero seed with no final inversion maps all-zero input to zero, which is
        // why Outlook's empty density list page carries a CRC of 0.
        assert_eq!(crc32(&[0; 496]), 0);
    }

    #[test]
    fn matches_zlib_relationship() {
        // zlib's crc32(data) == !raw(data) when seeded with !0; check a known vector.
        // raw("123456789") with seed 0 and no final xor.
        assert_eq!(crc32(b"123456789"), 0x2DFD_2D88);
    }
}
