//! The named property map (MS-PST 2.4.7), stored in NID_NAME_TO_ID_MAP.

use std::collections::BTreeMap;

use crate::crc::crc32;
use crate::ltp::PropValue;

const BUCKET_COUNT: u32 = 251;
const FIRST_NAMED_PROP_ID: u16 = 0x8000;

pub type Guid = [u8; 16];

/// Builds a GUID from its canonical parts, stored little-endian as on disk.
pub const fn guid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> Guid {
    let a = d1.to_le_bytes();
    let b = d2.to_le_bytes();
    let c = d3.to_le_bytes();
    [
        a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d4[0], d4[1], d4[2], d4[3], d4[4], d4[5],
        d4[6], d4[7],
    ]
}

const MAPI_TAIL: [u8; 8] = [0xC0, 0, 0, 0, 0, 0, 0, 0x46];
pub const PS_MAPI: Guid = guid(0x0002_0328, 0, 0, MAPI_TAIL);
pub const PS_PUBLIC_STRINGS: Guid = guid(0x0002_0329, 0, 0, MAPI_TAIL);
pub const PSETID_APPOINTMENT: Guid = guid(0x0006_2002, 0, 0, MAPI_TAIL);
pub const PS_INTERNET_HEADERS: Guid = guid(0x0002_0386, 0, 0, MAPI_TAIL);
pub const PSETID_COMMON: Guid = guid(0x0006_2008, 0, 0, MAPI_TAIL);
pub const PSETID_MESSAGING: Guid = guid(
    0x41F28F13,
    0x83F4,
    0x4114,
    [0xA5, 0x84, 0xEE, 0xDB, 0x5A, 0x6B, 0x0B, 0xFF],
);
pub const PSETID_MEETING: Guid = guid(
    0x6ED8DA90,
    0x450B,
    0x101B,
    [0x98, 0xDA, 0x00, 0xAA, 0x00, 0x3F, 0x13, 0x05],
);
pub const PSETID_SHARING: Guid = guid(0x0006_2014, 0, 0, MAPI_TAIL);
pub const PSETID_TASK: Guid = guid(0x0006_2003, 0, 0, MAPI_TAIL);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PropName {
    Id(u32),
    String(String),
}

/// Named properties registered when Outlook creates a store, in Outlook's order.
fn outlook_baseline() -> Vec<(Guid, PropName)> {
    use PropName::{Id, String as S};
    let s = |n: &str| S(n.to_owned());
    vec![
        (PSETID_APPOINTMENT, Id(0x8205)),
        (PSETID_APPOINTMENT, Id(0x8223)),
        (PSETID_APPOINTMENT, Id(0x8231)),
        (PSETID_APPOINTMENT, Id(0x8216)),
        (PSETID_APPOINTMENT, Id(0x820D)),
        (PSETID_APPOINTMENT, Id(0x820E)),
        (PSETID_APPOINTMENT, Id(0x8235)),
        (PSETID_APPOINTMENT, Id(0x8236)),
        (PSETID_APPOINTMENT, Id(0x8233)),
        (PS_INTERNET_HEADERS, s("content-class")),
        (PS_PUBLIC_STRINGS, s("DRMLicense")),
        (PS_INTERNET_HEADERS, s("x-cr-hashedpuzzle")),
        (PS_INTERNET_HEADERS, s("x-cr-puzzleid")),
        (PSETID_COMMON, Id(0x8580)),
        (PSETID_COMMON, Id(0x8581)),
        (
            PS_PUBLIC_STRINGS,
            s("urn:schemas-microsoft-com:office:outlook#storetypeprivate"),
        ),
        (PSETID_COMMON, Id(0x8510)),
        (PSETID_COMMON, Id(0x8578)),
        (PSETID_MESSAGING, s("IsMentioned")),
        (PS_PUBLIC_STRINGS, s("Keywords")),
        (PSETID_SHARING, Id(0x8F05)),
        (PSETID_COMMON, Id(0x8514)),
        (PSETID_SHARING, Id(0x8F07)),
        (PSETID_COMMON, Id(0x8503)),
        (PSETID_COMMON, Id(0x85A8)),
        (PSETID_COMMON, Id(0x8502)),
        (PSETID_TASK, Id(0x8104)),
        (PSETID_TASK, Id(0x8105)),
        (PSETID_TASK, Id(0x811C)),
        (
            PS_INTERNET_HEADERS,
            s("x-ms-exchange-organization-authdomain"),
        ),
        (PS_PUBLIC_STRINGS, s("DoNotForward")),
        (PSETID_COMMON, Id(0x8570)),
        (PSETID_TASK, Id(0x8101)),
        (PSETID_COMMON, Id(0x85E0)),
        (PSETID_COMMON, Id(0x8552)),
    ]
}

/// Assigns property IDs (0x8000 and up) to named properties.
pub struct NamedPropertyMap {
    entries: Vec<(Guid, PropName)>,
    index: std::collections::HashMap<(Guid, PropName), u16>,
}

impl Default for NamedPropertyMap {
    fn default() -> Self {
        let mut map = NamedPropertyMap {
            entries: Vec::new(),
            index: std::collections::HashMap::new(),
        };
        for (g, name) in outlook_baseline() {
            map.id(g, name);
        }
        map
    }
}

impl NamedPropertyMap {
    /// The property ID for a named property, registering it on first use.
    pub fn id(&mut self, guid: Guid, name: PropName) -> u16 {
        if let Some(&id) = self.index.get(&(guid, name.clone())) {
            return id;
        }
        let id = FIRST_NAMED_PROP_ID + self.entries.len() as u16;
        self.entries.push((guid, name.clone()));
        self.index.insert((guid, name), id);
        id
    }

    /// The properties of the name-to-ID map node.
    pub fn build(&self) -> BTreeMap<u16, PropValue> {
        let mut guids: Vec<Guid> = Vec::new();
        let mut entry_stream = Vec::new();
        let mut string_stream = Vec::new();
        let mut buckets: BTreeMap<u32, Vec<u8>> = BTreeMap::new();

        for (i, (g, name)) in self.entries.iter().enumerate() {
            let guid_index: u16 = match *g {
                PS_MAPI => 1,
                PS_PUBLIC_STRINGS => 2,
                other => {
                    let pos = guids.iter().position(|x| *x == other).unwrap_or_else(|| {
                        guids.push(other);
                        guids.len() - 1
                    });
                    3 + pos as u16
                }
            };
            let (key, hash_key, is_string) = match name {
                PropName::Id(id) => (*id, *id, false),
                PropName::String(s) => {
                    let utf16: Vec<u8> = s.encode_utf16().flat_map(u16::to_le_bytes).collect();
                    let offset = string_stream.len() as u32;
                    string_stream.extend_from_slice(&(utf16.len() as u32).to_le_bytes());
                    string_stream.extend_from_slice(&utf16);
                    string_stream.resize(string_stream.len().next_multiple_of(4), 0);
                    (offset, crc32(&utf16), true)
                }
            };
            let w_guid = (guid_index << 1) | u16::from(is_string);
            let record = |id: u32| {
                let mut r = id.to_le_bytes().to_vec();
                r.extend_from_slice(&w_guid.to_le_bytes());
                r.extend_from_slice(&(i as u16).to_le_bytes());
                r
            };
            entry_stream.extend(record(key));
            let bucket = (hash_key ^ u32::from(w_guid)) % BUCKET_COUNT;
            buckets.entry(bucket).or_default().extend(record(hash_key));
        }

        let mut props = BTreeMap::new();
        props.insert(0x0001, PropValue::Integer32(BUCKET_COUNT as i32));
        props.insert(0x0002, PropValue::Binary(guids.concat()));
        props.insert(0x0003, PropValue::Binary(entry_stream));
        props.insert(0x0004, PropValue::Binary(string_stream));
        for (bucket, records) in buckets {
            props.insert(0x1000 + bucket as u16, PropValue::Binary(records));
        }
        props
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bucket(props: &BTreeMap<u16, PropValue>, n: u16) -> Vec<u8> {
        match &props[&(0x1000 + n)] {
            PropValue::Binary(b) => b.clone(),
            _ => panic!("bucket is not binary"),
        }
    }

    /// Spot checks against the map in the blank PST Outlook creates.
    #[test]
    fn matches_outlook_blank_store() {
        let props = NamedPropertyMap::default().build();
        assert_eq!(props.keys().filter(|&&k| k >= 0x1000).count(), 34);
        // PSETID_Appointment 0x8205, guid index 3, property index 0.
        assert_eq!(bucket(&props, 151), [0x05, 0x82, 0, 0, 0x06, 0, 0, 0]);
        // "content-class" in PS_INTERNET_HEADERS: CRC of the name, string flag set.
        assert_eq!(bucket(&props, 206), [0x65, 0x8F, 0x00, 0xE6, 0x09, 0, 9, 0]);
        // Two properties sharing one bucket, in registration order.
        assert_eq!(
            bucket(&props, 164),
            [
                0x16, 0x82, 0, 0, 0x06, 0, 3, 0, 0x05, 0x81, 0, 0, 0x10, 0, 27, 0
            ]
        );
        match &props[&0x0002] {
            PropValue::Binary(g) => {
                assert_eq!(g.len(), 6 * 16);
                assert_eq!(&g[..16], &PSETID_APPOINTMENT);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn reuses_ids() {
        let mut map = NamedPropertyMap::default();
        let a = map.id(PS_INTERNET_HEADERS, PropName::String("x-mailer".into()));
        assert_eq!(a, 0x8000 + 35);
        assert_eq!(
            map.id(PS_INTERNET_HEADERS, PropName::String("x-mailer".into())),
            a
        );
    }
}
