//! Decoding of TNEF streams (`winmail.dat`, application/ms-tnef), MS-OXTNEF.
//!
//! Outlook wraps its MAPI properties in TNEF when it sends rich mail, meeting
//! responses and so on over SMTP. The properties are copied onto the message,
//! which restores the original message class, RTF body, meeting properties
//! and attachments.

use std::fmt;

use crate::ltp::{PropValue, prop_type};
use crate::messaging::{Attachment, Message, PropKey};
use crate::named_props::PropName;

const SIGNATURE: u32 = 0x223E_9F78;
const LEVEL_MESSAGE: u8 = 0x01;
const LEVEL_ATTACHMENT: u8 = 0x02;

// Attribute IDs (low 16 bits of the attribute ID; the high bits are its type).
const ATT_ATTACH_DATA: u16 = 0x800F;
const ATT_ATTACH_TITLE: u16 = 0x8010;
const ATT_BODY: u16 = 0x800C;
const ATT_MESSAGE_CLASS: u16 = 0x8008;
const ATT_ATTACH_REND_DATA: u16 = 0x9002;
const ATT_MAPI_PROPS: u16 = 0x9003;
const ATT_ATTACHMENT: u16 = 0x9005;
const ATT_OEM_CODEPAGE: u16 = 0x9007;

const PT_NULL: u16 = 0x0001;
const PT_OBJECT: u16 = 0x000D;

const PR_MESSAGE_CLASS: u16 = 0x001A;
const PR_BODY: u16 = 0x1000;
const PR_HTML: u16 = 0x1013;
const PR_RTF_COMPRESSED: u16 = 0x1009;
const PR_ATTACH_DATA: u16 = 0x3701;
const PR_ATTACH_FILENAME: u16 = 0x3704;
const PR_ATTACH_METHOD: u16 = 0x3705;
const PR_ATTACH_LONG_FILENAME: u16 = 0x3707;
const PR_ATTACH_MIME_TAG: u16 = 0x370E;
const PR_ATTACH_CONTENT_ID: u16 = 0x3712;
const PR_ATTACHMENT_HIDDEN: u16 = 0x7FFE;

/// Properties that describe the sender's copy (store identity, sizes, flags)
/// rather than the message, or that the PST writer computes itself.
const MESSAGE_SKIP: &[u16] = &[
    0x0E07, // PidTagMessageFlags
    0x0E08, // PidTagMessageSize
    0x0E09, // PidTagParentEntryId
    0x0E12, // PidTagMessageRecipients
    0x0E13, // PidTagMessageAttachments
    0x0E1B, // PidTagHasAttachments
    0x0FF6, // PidTagInstanceKey
    0x0FF8, // PidTagMappingSignature
    0x0FF9, // PidTagRecordKey
    0x0FFA, // PidTagStoreRecordKey
    0x0FFB, // PidTagStoreEntryId
    0x0FFE, // PidTagObjectType
    0x0FFF, // PidTagEntryId
    0x300B, // PidTagSearchKey
    0x340D, // PidTagStoreSupportMask
    0x340F, // PidTagStoreState
];

const ATTACHMENT_SKIP: &[u16] = &[
    0x0E20, // PidTagAttachSize
    0x0E21, // PidTagAttachNumber
    0x0FF9, // PidTagRecordKey
    0x0FFE, // PidTagObjectType
    0x0FFF, // PidTagEntryId
    PR_ATTACH_DATA,
    PR_ATTACH_METHOD,
];

#[derive(Debug)]
pub struct TnefError(String);

impl fmt::Display for TnefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid TNEF: {}", self.0)
    }
}

impl std::error::Error for TnefError {}

fn err<T>(msg: impl Into<String>) -> Result<T, TnefError> {
    Err(TnefError(msg.into()))
}

#[derive(Debug, Default)]
pub struct Tnef {
    /// From attMessageClass, with legacy class names translated.
    pub message_class: Option<String>,
    pub body: Option<String>,
    pub props: Vec<(PropKey, PropValue)>,
    pub attachments: Vec<TnefAttachment>,
}

#[derive(Debug, Default)]
pub struct TnefAttachment {
    pub title: Option<String>,
    pub data: Option<Vec<u8>>,
    pub props: Vec<(PropKey, PropValue)>,
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], TnefError> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.data.len());
        let Some(end) = end else {
            return err(format!("truncated at offset {}", self.pos));
        };
        let out = &self.data[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8, TnefError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, TnefError> {
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().expect("2 bytes"),
        ))
    }

    fn u32(&mut self) -> Result<u32, TnefError> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }

    fn at_end(&self) -> bool {
        self.pos >= self.data.len()
    }

    /// Skips to the next multiple of 4 from the start of the property block.
    fn align4(&mut self, start: usize) -> Result<(), TnefError> {
        let rel = self.pos - start;
        self.take(rel.next_multiple_of(4) - rel).map(|_| ())
    }
}

/// Translates the class names that TNEF uses for compatibility with old clients.
fn modern_class(class: &str) -> String {
    match class {
        "IPM.Microsoft Mail.Note" => "IPM.Note",
        "IPM.Microsoft Mail.Read Receipt" => "Report.IPM.Note.IPNRN",
        "IPM.Microsoft Mail.Non-Delivery" => "Report.IPM.Note.NDR",
        "IPM.Microsoft Schedule.MtgReq" => "IPM.Schedule.Meeting.Request",
        "IPM.Microsoft Schedule.MtgRespP" => "IPM.Schedule.Meeting.Resp.Pos",
        "IPM.Microsoft Schedule.MtgRespN" => "IPM.Schedule.Meeting.Resp.Neg",
        "IPM.Microsoft Schedule.MtgRespA" => "IPM.Schedule.Meeting.Resp.Tent",
        "IPM.Microsoft Schedule.MtgCncl" => "IPM.Schedule.Meeting.Canceled",
        other => other,
    }
    .to_owned()
}

fn string8(bytes: &[u8], codepage: u32) -> String {
    let bytes = bytes.split(|&b| b == 0).next().unwrap_or_default();
    let encoding = codepage::to_encoding(codepage as u16).unwrap_or(encoding_rs::WINDOWS_1252);
    encoding.decode_without_bom_handling(bytes).0.into_owned()
}

fn unicode(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&c| u16::from_le_bytes(c))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// Size of a fixed-size property type, or `None` for variable-size types.
fn fixed_size(ptype: u16) -> Option<usize> {
    use prop_type as t;
    Some(match ptype {
        PT_NULL => 0,
        t::INTEGER16 => 2,
        t::INTEGER32 | t::FLOATING32 | t::ERROR_CODE | t::BOOLEAN => 4,
        t::FLOATING64 | t::CURRENCY | t::FLOATING_TIME | t::INTEGER64 | t::TIME => 8,
        t::GUID => 16,
        _ => return None,
    })
}

fn fixed_value(ptype: u16, b: &[u8]) -> Option<PropValue> {
    use prop_type as t;
    let i16v = || i16::from_le_bytes([b[0], b[1]]);
    let i32v = || i32::from_le_bytes(b[..4].try_into().expect("4 bytes"));
    let i64v = || i64::from_le_bytes(b[..8].try_into().expect("8 bytes"));
    let f64v = || f64::from_le_bytes(b[..8].try_into().expect("8 bytes"));
    Some(match ptype {
        t::INTEGER16 => PropValue::Integer16(i16v()),
        t::INTEGER32 => PropValue::Integer32(i32v()),
        t::FLOATING32 => {
            PropValue::Floating32(f32::from_le_bytes(b[..4].try_into().expect("4 bytes")))
        }
        t::ERROR_CODE => PropValue::ErrorCode(i32v()),
        t::BOOLEAN => PropValue::Boolean(i32v() != 0),
        t::FLOATING64 => PropValue::Floating64(f64v()),
        t::CURRENCY => PropValue::Currency(i64v()),
        t::FLOATING_TIME => PropValue::FloatingTime(f64v()),
        t::INTEGER64 => PropValue::Integer64(i64v()),
        t::TIME => PropValue::Time(i64v()),
        t::GUID => PropValue::Guid(b[..16].try_into().expect("16 bytes")),
        _ => return None,
    })
}

/// A decoded MAPI property. Objects keep their raw data (after the IID).
enum Value {
    Prop(PropValue),
    Object(Vec<u8>),
    Null,
}

/// Parses an attMAPIProps/attAttachment block (MS-OXTNEF 2.1.3.4).
fn parse_props(block: &[u8], codepage: u32) -> Result<Vec<(PropKey, Value)>, TnefError> {
    let mut r = Reader {
        data: block,
        pos: 0,
    };
    let count = r.u32()?;
    let mut out = Vec::new();
    for _ in 0..count {
        let ptype = r.u16()?;
        let id = r.u16()?;
        let key = if id >= 0x8000 {
            let guid: [u8; 16] = r.take(16)?.try_into().expect("16 bytes");
            let name = match r.u32()? {
                0 => PropName::Id(r.u32()?),
                1 => {
                    let len = r.u32()? as usize;
                    let name = unicode(r.take(len)?);
                    r.align4(0)?;
                    PropName::String(name)
                }
                kind => return err(format!("unknown named property kind {kind}")),
            };
            PropKey::Named(guid, name)
        } else {
            PropKey::Id(id)
        };

        let base = ptype & !prop_type::MULTIPLE;
        let multi = ptype & prop_type::MULTIPLE != 0;
        let value = if let Some(size) = fixed_size(base) {
            let n = if multi { r.u32()? as usize } else { 1 };
            let mut values = Vec::with_capacity(n);
            for _ in 0..n {
                let b = r.take(size)?;
                r.align4(0)?;
                values.push(b);
            }
            if base == PT_NULL {
                Value::Null
            } else if !multi {
                Value::Prop(fixed_value(base, values[0]).expect("fixed type"))
            } else {
                use prop_type as t;
                let v: Vec<PropValue> =
                    values.iter().filter_map(|b| fixed_value(base, b)).collect();
                match base {
                    t::INTEGER16 => Value::Prop(PropValue::MultipleInteger16(
                        v.iter()
                            .map(|x| {
                                if let PropValue::Integer16(i) = x {
                                    *i
                                } else {
                                    0
                                }
                            })
                            .collect(),
                    )),
                    t::INTEGER32 => Value::Prop(PropValue::MultipleInteger32(
                        v.iter()
                            .map(|x| {
                                if let PropValue::Integer32(i) = x {
                                    *i
                                } else {
                                    0
                                }
                            })
                            .collect(),
                    )),
                    t::INTEGER64 => Value::Prop(PropValue::MultipleInteger64(
                        v.iter()
                            .map(|x| {
                                if let PropValue::Integer64(i) = x {
                                    *i
                                } else {
                                    0
                                }
                            })
                            .collect(),
                    )),
                    t::TIME => Value::Prop(PropValue::MultipleTime(
                        v.iter()
                            .map(|x| if let PropValue::Time(i) = x { *i } else { 0 })
                            .collect(),
                    )),
                    t::GUID => Value::Prop(PropValue::MultipleGuid(
                        v.iter()
                            .map(|x| {
                                if let PropValue::Guid(g) = x {
                                    *g
                                } else {
                                    [0; 16]
                                }
                            })
                            .collect(),
                    )),
                    // Other multi-valued types are rare; drop them.
                    _ => Value::Null,
                }
            }
        } else {
            let n = r.u32()? as usize;
            let mut items = Vec::with_capacity(n.min(1024));
            for _ in 0..n {
                let len = r.u32()? as usize;
                items.push(r.take(len)?);
                r.align4(0)?;
            }
            let first = items.first().copied().unwrap_or_default();
            match (base, multi) {
                (prop_type::STRING8, false) => {
                    Value::Prop(PropValue::Unicode(string8(first, codepage)))
                }
                (prop_type::UNICODE, false) => Value::Prop(PropValue::Unicode(unicode(first))),
                (prop_type::BINARY, false) => Value::Prop(PropValue::Binary(first.to_vec())),
                (prop_type::STRING8, true) => Value::Prop(PropValue::MultipleUnicode(
                    items.iter().map(|b| string8(b, codepage)).collect(),
                )),
                (prop_type::UNICODE, true) => Value::Prop(PropValue::MultipleUnicode(
                    items.iter().map(|b| unicode(b)).collect(),
                )),
                (prop_type::BINARY, true) => Value::Prop(PropValue::MultipleBinary(
                    items.iter().map(|b| b.to_vec()).collect(),
                )),
                // Object data starts with the interface ID of the object.
                (PT_OBJECT, false) => Value::Object(first.get(16..).unwrap_or_default().to_vec()),
                _ => return err(format!("unsupported property type {ptype:#06x}")),
            }
        };
        out.push((key, value));
    }
    Ok(out)
}

pub fn parse(data: &[u8]) -> Result<Tnef, TnefError> {
    let mut r = Reader { data, pos: 0 };
    if r.u32()? != SIGNATURE {
        return err("bad signature");
    }
    r.u16()?; // legacy key
    let mut tnef = Tnef::default();
    let mut codepage = 1252;
    while !r.at_end() {
        let level = r.u8()?;
        let id = r.u32()? as u16;
        let len = r.u32()? as usize;
        let body = r.take(len)?;
        r.u16()?; // checksum
        match (level, id) {
            (LEVEL_MESSAGE, ATT_OEM_CODEPAGE) if body.len() >= 4 => {
                codepage = u32::from_le_bytes(body[..4].try_into().expect("4 bytes"));
            }
            (LEVEL_MESSAGE, ATT_MESSAGE_CLASS) => {
                tnef.message_class = Some(modern_class(&string8(body, codepage)));
            }
            (LEVEL_MESSAGE, ATT_BODY) => tnef.body = Some(string8(body, codepage)),
            (LEVEL_MESSAGE, ATT_MAPI_PROPS) => {
                for (key, value) in parse_props(body, codepage)? {
                    if let Value::Prop(v) = value {
                        tnef.props.push((key, v));
                    }
                }
            }
            (LEVEL_ATTACHMENT, ATT_ATTACH_REND_DATA) => {
                tnef.attachments.push(TnefAttachment::default())
            }
            (LEVEL_ATTACHMENT, _) => {
                let Some(att) = tnef.attachments.last_mut() else {
                    return err("attachment attribute before attRenddata");
                };
                match id {
                    ATT_ATTACH_TITLE => att.title = Some(string8(body, codepage)),
                    ATT_ATTACH_DATA => att.data = Some(body.to_vec()),
                    ATT_ATTACHMENT => {
                        for (key, value) in parse_props(body, codepage)? {
                            match (key, value) {
                                (PropKey::Id(PR_ATTACH_DATA), Value::Object(d)) => {
                                    att.data = Some(d)
                                }
                                (
                                    PropKey::Id(PR_ATTACH_DATA),
                                    Value::Prop(PropValue::Binary(d)),
                                ) => att.data = Some(d),
                                (key, Value::Prop(v)) => att.props.push((key, v)),
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    Ok(tnef)
}

fn prop(props: &[(PropKey, PropValue)], id: u16) -> Option<&PropValue> {
    props
        .iter()
        .find_map(|(k, v)| (*k == PropKey::Id(id)).then_some(v))
}

fn text(props: &[(PropKey, PropValue)], id: u16) -> Option<String> {
    match prop(props, id) {
        Some(PropValue::Unicode(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// Merges a decoded TNEF stream into `msg`: its message class and properties
/// override the MIME mapping (except bodies the MIME message already has),
/// and its attachments are added.
pub fn apply(msg: &mut Message, tnef: Tnef) {
    if let Some(class) = text(&tnef.props, PR_MESSAGE_CLASS).or(tnef.message_class) {
        msg.message_class = Some(modern_class(&class));
    }
    if msg.body.is_none() {
        msg.body = text(&tnef.props, PR_BODY).or(tnef.body);
    }
    let has_html = msg.html.is_some();
    for (key, value) in tnef.props {
        if let PropKey::Id(id) = key {
            let mime_body = (id == PR_BODY) || (id == PR_HTML && has_html);
            // An empty body property is no body; readers reject it.
            let empty_body = matches!(&value, PropValue::Binary(b) if b.is_empty())
                && matches!(id, PR_HTML | PR_RTF_COMPRESSED);
            if mime_body || empty_body || id == PR_MESSAGE_CLASS || MESSAGE_SKIP.contains(&id) {
                continue;
            }
        }
        msg.extra_props.push((key, value));
    }
    for att in tnef.attachments {
        // Attachments by reference carry no data and cannot be stored.
        let Some(data) = att.data else { continue };
        let filename = text(&att.props, PR_ATTACH_LONG_FILENAME)
            .or_else(|| text(&att.props, PR_ATTACH_FILENAME))
            .or(att.title);
        msg.attachments.push(Attachment {
            filename,
            mime_type: text(&att.props, PR_ATTACH_MIME_TAG),
            content_id: text(&att.props, PR_ATTACH_CONTENT_ID),
            inline: matches!(
                prop(&att.props, PR_ATTACHMENT_HIDDEN),
                Some(PropValue::Boolean(true))
            ),
            data,
            embedded: None,
            extra_props: att
                .props
                .into_iter()
                .filter(|(k, _)| !matches!(k, PropKey::Id(id) if ATTACHMENT_SKIP.contains(id)))
                .collect(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::named_props::PSETID_APPOINTMENT;

    fn attr(level: u8, id: u32, data: &[u8]) -> Vec<u8> {
        let mut b = vec![level];
        b.extend_from_slice(&id.to_le_bytes());
        b.extend_from_slice(&(data.len() as u32).to_le_bytes());
        b.extend_from_slice(data);
        let sum: u32 = data.iter().map(|&x| u32::from(x)).sum();
        b.extend_from_slice(&(sum as u16).to_le_bytes());
        b
    }

    fn pad(b: &mut Vec<u8>) {
        b.resize(b.len().next_multiple_of(4), 0);
    }

    fn unicode_value(b: &mut Vec<u8>, s: &str) {
        let data: Vec<u8> = s
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&(data.len() as u32).to_le_bytes());
        b.extend_from_slice(&data);
        pad(b);
    }

    #[test]
    fn decodes_meeting_response_stream() {
        let mut props = 3u32.to_le_bytes().to_vec();
        props.extend_from_slice(&[0x1F, 0x00, 0x37, 0x00]); // PR_SUBJECT, Unicode
        unicode_value(&mut props, "Accepted: Sync");
        // Named: PSETID_Appointment LID 0x8205 (busy status), Integer32.
        props.extend_from_slice(&[0x03, 0x00, 0x00, 0x80]);
        props.extend_from_slice(&PSETID_APPOINTMENT);
        props.extend_from_slice(&0u32.to_le_bytes());
        props.extend_from_slice(&0x8205u32.to_le_bytes());
        props.extend_from_slice(&2i32.to_le_bytes());
        props.extend_from_slice(&[0x1E, 0x00, 0x08, 0x10]); // a String8 property
        props.extend_from_slice(&1u32.to_le_bytes());
        props.extend_from_slice(&4u32.to_le_bytes());
        props.extend_from_slice(b"Gr\xFC\0"); // windows-1252 u-umlaut
        let mut att_props = 1u32.to_le_bytes().to_vec();
        att_props.extend_from_slice(&[0x1F, 0x00, 0x07, 0x37]); // PR_ATTACH_LONG_FILENAME
        unicode_value(&mut att_props, "long name.txt");

        let mut stream = SIGNATURE.to_le_bytes().to_vec();
        stream.extend_from_slice(&0x1234u16.to_le_bytes());
        stream.extend(attr(LEVEL_MESSAGE, 0x0006_9007, &1252u32.to_le_bytes()));
        stream.extend(attr(
            LEVEL_MESSAGE,
            0x0007_8008,
            b"IPM.Microsoft Schedule.MtgRespP\0",
        ));
        stream.extend(attr(LEVEL_MESSAGE, 0x0006_9003, &props));
        stream.extend(attr(LEVEL_ATTACHMENT, 0x0006_9002, &[0; 14]));
        stream.extend(attr(LEVEL_ATTACHMENT, 0x0001_8010, b"LONGNA~1.TXT\0"));
        stream.extend(attr(LEVEL_ATTACHMENT, 0x0006_800F, b"hello"));
        stream.extend(attr(LEVEL_ATTACHMENT, 0x0006_9005, &att_props));

        let tnef = parse(&stream).unwrap();
        assert_eq!(
            tnef.message_class.as_deref(),
            Some("IPM.Schedule.Meeting.Resp.Pos")
        );
        assert_eq!(
            tnef.props[0],
            (
                PropKey::Id(0x0037),
                PropValue::Unicode("Accepted: Sync".into())
            )
        );
        assert_eq!(
            tnef.props[1],
            (
                PropKey::Named(PSETID_APPOINTMENT, PropName::Id(0x8205)),
                PropValue::Integer32(2)
            )
        );
        assert_eq!(tnef.props[2].1, PropValue::Unicode("Grü".into()));

        let mut msg = Message::default();
        apply(&mut msg, tnef);
        assert_eq!(
            msg.message_class.as_deref(),
            Some("IPM.Schedule.Meeting.Resp.Pos")
        );
        assert_eq!(msg.attachments.len(), 1);
        assert_eq!(
            msg.attachments[0].filename.as_deref(),
            Some("long name.txt")
        );
        assert_eq!(msg.attachments[0].data, b"hello");
        assert!(parse(b"not tnef").is_err());
    }
}
