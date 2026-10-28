//! RTF bodies: HTML encapsulated in RTF (MS-OXRTFEX) and the LZFu
//! compression of PidTagRtfCompressed (MS-OXRTFCP).

use crate::crc::crc32;

/// The dictionary every LZFu stream starts from (MS-OXRTFCP 2.1.3.1.1).
const PRESET: &[u8] = b"{\\rtf1\\ansi\\mac\\deff0\\deftab720{\\fonttbl;}{\\f0\\fnil \\froman \\fswiss \\fmodern \\fscript \\fdecor MS Sans SerifSymbolArialTimes New RomanCourier{\\colortbl\\red0\\green0\\blue0\r\n\\par \\pard\\plain\\f0\\fs20\\b\\i\\u\\tab\\tx";
const WINDOW: usize = 4096;
const MIN_MATCH: usize = 2;
const MAX_MATCH: usize = 17;
/// Candidates examined per position; bounds compression time.
const MAX_CHAIN: usize = 256;
const COMPRESSED: u32 = 0x7546_5A4C; // "LZFu"
const UNCOMPRESSED: u32 = 0x414C_454D; // "MELA"

/// Compresses RTF into the PidTagRtfCompressed format.
pub fn compress(raw: &[u8]) -> Vec<u8> {
    // The stream as the decompressor sees it: the preset, then the input.
    let mut logical = Vec::with_capacity(PRESET.len() + raw.len());
    logical.extend_from_slice(PRESET);
    logical.extend_from_slice(raw);
    let key = |i: usize| (usize::from(logical[i]) << 8) | usize::from(logical[i + 1]);
    let mut heads: std::collections::HashMap<usize, Vec<usize>> = std::collections::HashMap::new();
    for i in 0..PRESET.len() - 1 {
        heads.entry(key(i)).or_default().push(i);
    }

    let mut payload = Vec::with_capacity(raw.len() / 2 + 16);
    let mut control_at = payload.len();
    payload.push(0u8);
    let mut bit = 0;
    let mut emit = |payload: &mut Vec<u8>, reference: Option<u16>, literal: u8| {
        if bit == 8 {
            control_at = payload.len();
            payload.push(0);
            bit = 0;
        }
        match reference {
            Some(r) => {
                payload[control_at] |= 1 << bit;
                payload.extend_from_slice(&r.to_be_bytes());
            }
            None => payload.push(literal),
        }
        bit += 1;
    };

    let mut cur = PRESET.len();
    while cur < logical.len() {
        let mut best = (0usize, 0usize); // (length, logical position)
        if cur + MIN_MATCH <= logical.len()
            && let Some(chain) = heads.get(&key(cur))
        {
            // Like the specification's reference compressor, scan from
            // the oldest position in the window and keep the first of
            // the longest matches.
            let oldest = cur.saturating_sub(WINDOW - 1);
            let start = chain.partition_point(|&q| q < oldest);
            for &q in chain[start..].iter().take(MAX_CHAIN) {
                let max = MAX_MATCH.min(logical.len() - cur);
                let len = (0..max)
                    .take_while(|&k| logical[q + k] == logical[cur + k])
                    .count();
                if len > best.0 {
                    best = (len, q);
                    if len == MAX_MATCH {
                        break;
                    }
                }
            }
        }
        let step = if best.0 >= MIN_MATCH {
            let token = (((best.1 % WINDOW) as u16) << 4) | (best.0 - 2) as u16;
            emit(&mut payload, Some(token), 0);
            best.0
        } else {
            emit(&mut payload, None, logical[cur]);
            1
        };
        for i in cur..cur + step {
            if i + 1 < logical.len() {
                heads.entry(key(i)).or_default().push(i);
            }
        }
        cur += step;
    }
    // The end marker: a reference to the current write position.
    emit(&mut payload, Some(((cur % WINDOW) as u16) << 4), 0);

    let mut out = Vec::with_capacity(16 + payload.len());
    out.extend_from_slice(&(12 + payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&(raw.len() as u32).to_le_bytes());
    out.extend_from_slice(&COMPRESSED.to_le_bytes());
    out.extend_from_slice(&crc32(&payload).to_le_bytes());
    out.extend(payload);
    out
}

/// Decompresses PidTagRtfCompressed, checking its size and CRC.
pub fn decompress(data: &[u8]) -> Option<Vec<u8>> {
    let field = |i: usize| Some(u32::from_le_bytes(data.get(i..i + 4)?.try_into().ok()?));
    let (comp_size, raw_size, kind, crc) = (field(0)?, field(4)?, field(8)?, field(12)?);
    if comp_size as usize != data.len() - 4 {
        return None;
    }
    let payload = &data[16..];
    match kind {
        UNCOMPRESSED => return Some(payload.get(..raw_size as usize)?.to_vec()),
        COMPRESSED if crc32(payload) == crc => {}
        _ => return None,
    }
    let mut dict = [0u8; WINDOW];
    dict[..PRESET.len()].copy_from_slice(PRESET);
    let mut write = PRESET.len();
    let mut out = Vec::with_capacity(raw_size as usize);
    let mut i = 0;
    while i < payload.len() {
        let control = payload[i];
        i += 1;
        for bit in 0..8 {
            if i >= payload.len() {
                break;
            }
            if control & (1 << bit) == 0 {
                let c = payload[i];
                i += 1;
                out.push(c);
                dict[write % WINDOW] = c;
                write += 1;
                continue;
            }
            let token = u16::from_be_bytes([payload[i], *payload.get(i + 1)?]);
            i += 2;
            let offset = usize::from(token >> 4);
            if offset == write % WINDOW {
                return (out.len() == raw_size as usize).then_some(out);
            }
            for k in 0..usize::from(token & 0xF) + 2 {
                let c = dict[(offset + k) % WINDOW];
                out.push(c);
                dict[write % WINDOW] = c;
                write += 1;
            }
        }
    }
    (out.len() == raw_size as usize).then_some(out)
}

/// Appends `text` with RTF escapes: `\`, `{` and `}` escaped, line breaks as
/// `\par`, tabs as `\tab`, and characters beyond ASCII as `\uN?`.
fn escape(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '\\' | '{' | '}' => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push_str("\\par\r\n"),
            '\r' => {}
            '\t' => out.push_str("\\tab "),
            ' '..='~' => out.push(c),
            _ => {
                let mut units = [0u16; 2];
                for u in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{}?", *u as i16));
                }
            }
        }
    }
}

/// Wraps HTML in RTF so that de-encapsulation (MS-OXRTFEX 2.4.2) gives back
/// the same HTML: tags go into `\*\htmltag` destinations, text between them
/// stays RTF text.
pub fn encapsulate_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len() + html.len() / 4 + 128);
    out.push_str("{\\rtf1\\ansi\\ansicpg1252\\fromhtml1 \\deff0{\\fonttbl{\\f0\\fswiss\\fcharset0 Arial;}}\\uc1\r\n");
    let mut rest = html;
    while !rest.is_empty() {
        match rest.find('<') {
            Some(0) => {
                // A tag runs to the next '>' (comments to "-->").
                let end = if rest.starts_with("<!--") {
                    rest.find("-->").map(|i| i + 3)
                } else {
                    rest.find('>').map(|i| i + 1)
                }
                .unwrap_or(rest.len());
                out.push_str("{\\*\\htmltag64 ");
                escape(&mut out, &rest[..end]);
                out.push('}');
                rest = &rest[end..];
            }
            Some(i) => {
                escape(&mut out, &rest[..i]);
                rest = &rest[i..];
            }
            None => {
                escape(&mut out, rest);
                rest = "";
            }
        }
    }
    out.push('}');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MS-OXRTFCP 3.1.1: the specification's compressed example, byte for byte.
    #[test]
    fn matches_specification_example() {
        let raw = b"{\\rtf1\\ansi\\ansicpg1252\\pard hello world}\r\n";
        let expected = [
            0x2d, 0x00, 0x00, 0x00, 0x2b, 0x00, 0x00, 0x00, 0x4c, 0x5a, 0x46, 0x75, 0xf1, 0xc5,
            0xc7, 0xa7, 0x03, 0x00, 0x0a, 0x00, 0x72, 0x63, 0x70, 0x67, 0x31, 0x32, 0x35, 0x42,
            0x32, 0x0a, 0xf3, 0x20, 0x68, 0x65, 0x6c, 0x09, 0x00, 0x20, 0x62, 0x77, 0x05, 0xb0,
            0x6c, 0x64, 0x7d, 0x0a, 0x80, 0x0f, 0xa0,
        ];
        assert_eq!(compress(raw), expected);
        assert_eq!(decompress(&expected).unwrap(), raw);
    }

    #[test]
    fn round_trips_long_and_repetitive_input() {
        let mut raw = Vec::new();
        for i in 0..5000 {
            raw.extend_from_slice(
                format!("<p>line {i} \\par repeated text {}</p>\r\n", i % 7).as_bytes(),
            );
        }
        let c = compress(&raw);
        assert_eq!(decompress(&c).unwrap(), raw);
        assert!(c.len() < raw.len() / 3);
        assert_eq!(decompress(&compress(b"")).unwrap(), b"");
    }

    #[test]
    fn decompresses_outlook_blob_header_checks() {
        let mut c = compress(b"{\\rtf1 x}");
        c[20] ^= 0xFF; // corrupt the payload: the CRC no longer matches
        assert!(decompress(&c).is_none());
    }

    #[test]
    fn encapsulates_html() {
        let rtf = encapsulate_html("<p>Grüße {x}</p>\n<!-- a > b -->");
        assert!(rtf.starts_with("{\\rtf1\\ansi\\ansicpg1252\\fromhtml1 "));
        assert!(rtf.contains(
            "{\\*\\htmltag64 <p>}Gr\\u252?\\u223?e \\{x\\}{\\*\\htmltag64 </p>}\\par\r\n"
        ));
        assert!(rtf.contains("{\\*\\htmltag64 <!-- a > b -->}"));
        assert!(rtf.ends_with('}'));
    }
}
