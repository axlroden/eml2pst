//! Mapping from RFC 5322 / MIME messages to [`Message`].

use mail_parser::{AddressList, MessageParser, MessagePart};

type MimeMessage<'a> = mail_parser::Message<'a>;

use crate::messaging::{
    Attachment, Importance, Mailbox, Message, Recipient, RecipientKind, Sensitivity,
};
use crate::{meeting, tnef};

/// Seconds between 1601-01-01 (FILETIME epoch) and 1970-01-01.
const FILETIME_UNIX_OFFSET: i64 = 11_644_473_600;

pub fn unix_to_filetime(secs: i64) -> i64 {
    (secs + FILETIME_UNIX_OFFSET) * 10_000_000
}

/// Forwarded messages nested deeper than this stay .eml file attachments.
const MAX_EMBED_DEPTH: usize = 8;

/// Parses an .eml file. Returns `None` only when the input holds no message.
pub fn parse_eml(raw: &[u8]) -> Option<Message> {
    parse_at_depth(raw, 0)
}

fn parse_at_depth(raw: &[u8], depth: usize) -> Option<Message> {
    let parsed = MessageParser::new().parse(raw)?;
    let mut msg = Message {
        subject: subject(&parsed),
        from: parsed.from().and_then(|l| mailboxes(&l).into_iter().next()),
        read: true,
        ..Default::default()
    };
    // Sender differs from From when mail is sent on behalf of someone; Outlook
    // shows it as "Sender on behalf of From".
    let sender = parsed
        .sender()
        .and_then(|l| mailboxes(&l).into_iter().next());
    match (&msg.from, sender) {
        (None, sender) => msg.from = sender,
        (Some(from), Some(sender)) if !same_address(from, &sender) => msg.sender = Some(sender),
        _ => {}
    }

    for (list, kind) in [
        (parsed.to(), RecipientKind::To),
        (parsed.cc(), RecipientKind::Cc),
        (parsed.bcc(), RecipientKind::Bcc),
    ] {
        if let Some(list) = list {
            msg.recipients.extend(
                mailboxes(&list)
                    .into_iter()
                    .map(|mailbox| Recipient { kind, mailbox }),
            );
        }
    }

    let sent = parsed
        .date()
        .filter(|d| d.is_valid())
        .map(|d| unix_to_filetime(d.to_timestamp()));
    let received = parsed
        .received()
        .and_then(|r| r.date())
        .filter(|d| d.is_valid())
        .map(|d| unix_to_filetime(d.to_timestamp()));
    msg.sent_time = sent;
    msg.received_time = received.or(sent);

    let texts: Vec<String> = parsed
        .text_body()
        // A part without Content-Type is text/plain (RFC 2045 section 5.2).
        .filter(|p| p.content_type().is_none() || p.is_content_type("text", "plain"))
        .filter_map(|p| p.text().map(|t| t.into_owned()))
        .collect();
    let htmls: Vec<String> = parsed
        .html_body()
        .filter(|p| p.is_content_type("text", "html"))
        .filter_map(|p| p.html_utf8().map(|t| t.into_owned()))
        .filter(|h| !h.trim().is_empty())
        .collect();
    if !texts.is_empty() {
        msg.body = Some(texts.join("\r\n"));
    } else if !htmls.is_empty() {
        msg.body = Some(mail_parser::html_to_text(&htmls.join("\n")));
    }
    if !htmls.is_empty() {
        msg.html = Some(htmls.join("\n"));
    }

    let headers = parsed.root_part().raw_headers();
    if !headers.is_empty() {
        msg.transport_headers = Some(String::from_utf8_lossy(headers).into_owned());
    }
    msg.message_id = parsed.message_id().map(angle);
    msg.in_reply_to = parsed.in_reply_to().and_then(|l| l.first().map(angle));
    msg.references = parsed
        .references()
        .filter(|l| !l.is_empty())
        .map(|l| l.iter().map(angle).collect::<Vec<_>>().join(" "));

    msg.attachments = parsed
        .attachments()
        .enumerate()
        .map(|(i, p)| attachment(&p, i, depth))
        .collect();
    msg.importance = importance(&parsed);
    msg.sensitivity =
        header_text(&parsed, "Sensitivity").and_then(|v| match v.to_ascii_lowercase().as_str() {
            "personal" => Some(Sensitivity::Personal),
            "private" => Some(Sensitivity::Private),
            "company-confidential" => Some(Sensitivity::Confidential),
            _ => None,
        });
    smime(&parsed, &mut msg);
    unwrap_tnef(&mut msg);
    // TNEF already restored Outlook's own meeting object, if there was one.
    let is_meeting = msg
        .message_class
        .as_deref()
        .is_some_and(|c| c.starts_with("IPM.Schedule.Meeting."));
    if !is_meeting {
        meeting::apply(&mut msg);
    }
    Some(msg)
}

/// The trimmed value of the first header named `name`.
fn header_text(parsed: &MimeMessage<'_>, name: &str) -> Option<String> {
    parsed
        .root_part()
        .headers()
        .iter()
        .find(|h| h.raw_name().eq_ignore_ascii_case(name))
        .map(|h| String::from_utf8_lossy(h.raw_value()).trim().to_owned())
        .filter(|v| !v.is_empty())
}

/// Importance from Importance (RFC 2156), X-Priority, Priority (RFC 2156) or
/// X-MSMail-Priority, in that order of precedence.
fn importance(parsed: &MimeMessage<'_>) -> Option<Importance> {
    let word = |v: &str| match v.to_ascii_lowercase().as_str() {
        "high" | "urgent" => Some(Importance::High),
        "low" | "non-urgent" => Some(Importance::Low),
        "normal" => Some(Importance::Normal),
        _ => None,
    };
    header_text(parsed, "Importance")
        .and_then(|v| word(&v))
        .or_else(|| {
            let v = header_text(parsed, "X-Priority")?;
            match v.chars().next()? {
                '1' | '2' => Some(Importance::High),
                '3' => Some(Importance::Normal),
                '4' | '5' => Some(Importance::Low),
                _ => None,
            }
        })
        .or_else(|| header_text(parsed, "Priority").and_then(|v| word(&v)))
        .or_else(|| header_text(parsed, "X-MSMail-Priority").and_then(|v| word(&v)))
}

/// S/MIME as Outlook stores it (MS-OXOSMIME): a clear-signed message keeps
/// the whole multipart/signed entity in an smime.p7m attachment, so Outlook
/// can verify the signature; an encrypted or opaque-signed one keeps its
/// PKCS #7 data. The text already mapped stays for previews and search.
fn smime(parsed: &MimeMessage<'_>, msg: &mut Message) {
    let root = parsed.root_part();
    let (class, mime, data) = if root.is_content_type("multipart", "signed") {
        let mut entity = Vec::new();
        for h in root.headers().iter() {
            if h.raw_name().to_ascii_lowercase().starts_with("content-") {
                entity.extend_from_slice(h.raw_name().as_bytes());
                entity.extend_from_slice(b":");
                entity.extend_from_slice(h.raw_value());
            }
        }
        entity.extend_from_slice(b"\r\n");
        entity.extend_from_slice(root.raw_body());
        ("IPM.Note.SMIME.MultipartSigned", "multipart/signed", entity)
    } else if root.is_content_type("application", "pkcs7-mime")
        || root.is_content_type("application", "x-pkcs7-mime")
    {
        (
            "IPM.Note.SMIME",
            "application/pkcs7-mime",
            root.decoded().into_owned(),
        )
    } else {
        return;
    };
    msg.message_class = Some(class.to_owned());
    msg.attachments = vec![Attachment {
        filename: Some("smime.p7m".to_owned()),
        mime_type: Some(mime.to_owned()),
        data,
        ..Default::default()
    }];
}

fn is_tnef(att: &Attachment) -> bool {
    matches!(
        att.mime_type.as_deref(),
        Some("application/ms-tnef" | "application/vnd.ms-tnef")
    ) || att
        .filename
        .as_deref()
        .is_some_and(|f| f.eq_ignore_ascii_case("winmail.dat"))
}

/// Replaces a TNEF attachment (winmail.dat) by the properties and attachments
/// it carries. One that does not decode stays an ordinary attachment.
fn unwrap_tnef(msg: &mut Message) {
    let Some(index) = msg.attachments.iter().position(is_tnef) else {
        return;
    };
    if let Ok(decoded) = tnef::parse(&msg.attachments[index].data) {
        msg.attachments.remove(index);
        tnef::apply(msg, decoded);
    }
}

/// The decoded Subject. mail-parser trims surrounding whitespace; trailing
/// whitespace is part of the value as written, so it is put back.
fn subject(parsed: &MimeMessage<'_>) -> Option<String> {
    let decoded = parsed.subject()?.to_owned();
    let Some(header) = parsed
        .root_part()
        .headers()
        .iter()
        .find(|h| h.raw_name().eq_ignore_ascii_case("subject"))
    else {
        return Some(decoded);
    };
    let raw = String::from_utf8_lossy(header.raw_value());
    let unfolded = raw
        .trim_end_matches(['\r', '\n'])
        .replace("\r\n", "")
        .replace('\n', "");
    let trailing = &unfolded[unfolded.trim_end_matches([' ', '\t']).len()..];
    if trailing.is_empty() || decoded.ends_with(trailing) || decoded.is_empty() {
        Some(decoded)
    } else {
        Some(decoded + trailing)
    }
}

fn same_address(a: &Mailbox, b: &Mailbox) -> bool {
    match (&a.email, &b.email) {
        (Some(x), Some(y)) => x.eq_ignore_ascii_case(y),
        _ => a.name == b.name,
    }
}

/// A file extension for a MIME type, for parts that carry no file name.
fn extension(mime_type: &str) -> &'static str {
    match mime_type {
        "text/plain" | "message/delivery-status" | "text/rfc822-headers" => ".txt",
        "text/html" => ".html",
        "text/calendar" | "application/ics" => ".ics",
        "text/vcard" | "text/x-vcard" => ".vcf",
        "image/png" => ".png",
        "image/jpeg" | "image/jpg" => ".jpg",
        "image/gif" => ".gif",
        "application/pdf" => ".pdf",
        "application/ms-tnef" | "application/vnd.ms-tnef" => ".dat",
        "message/rfc822" => ".eml",
        _ => "",
    }
}

fn mailboxes(list: &AddressList<'_>) -> Vec<Mailbox> {
    list.mailboxes()
        .map(|m| Mailbox {
            name: m.name().map(str::to_owned).filter(|n| !n.is_empty()),
            email: m.address().map(str::to_owned).filter(|a| !a.is_empty()),
        })
        .filter(|m| m.name.is_some() || m.email.is_some())
        .collect()
}

fn angle(id: &str) -> String {
    format!("<{id}>")
}

fn attachment(part: &MessagePart<'_>, index: usize, depth: usize) -> Attachment {
    let mime_type = part.content_type().map(|ct| match ct.subtype() {
        Some(sub) => format!("{}/{}", ct.ctype(), sub).to_ascii_lowercase(),
        None => ct.ctype().to_ascii_lowercase(),
    });
    let filename = part
        .attachment_name()
        .map(str::to_owned)
        .unwrap_or_else(|| default_filename(part, mime_type.as_deref(), index));
    let content_id = part
        .content_id()
        .map(|c| c.trim_matches(|ch| ch == '<' || ch == '>').to_owned())
        .filter(|c| !c.is_empty());
    let data = part.decoded().into_owned();
    // A forwarded message becomes an Outlook item of its own.
    let embedded = (part.is_message() && depth < MAX_EMBED_DEPTH)
        .then(|| parse_at_depth(&data, depth + 1))
        .flatten()
        .map(Box::new);
    Attachment {
        inline: part.is_inline() && content_id.is_some(),
        filename: Some(filename),
        mime_type,
        content_id,
        data,
        embedded,
        extra_props: Vec::new(),
    }
}

/// A name for a part that has none. Forwarded messages are named after their
/// subject so they open as .eml files; other parts get an extension that
/// matches their type.
fn default_filename(part: &MessagePart<'_>, mime_type: Option<&str>, index: usize) -> String {
    if part.is_message() {
        let subject = part
            .nested()
            .and_then(|m| m.subject().map(str::to_owned))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "message".to_owned());
        return format!("{}.eml", sanitize_filename(&subject));
    }
    match extension(mime_type.unwrap_or_default()) {
        ".ics" => "invite.ics".to_owned(),
        ".dat" => "winmail.dat".to_owned(),
        ext => format!("attachment{}{ext}", index + 1),
    }
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .take(120)
        .collect::<String>()
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_headers_bodies_and_attachments() {
        let raw = b"From: Ann Example <ann@example.com>\r\n\
To: Bob <bob@example.com>, carol@example.com\r\n\
Cc: Dan <dan@example.com>\r\n\
Subject: =?UTF-8?Q?Gr=C3=BC=C3=9Fe?=\r\n\
Date: Tue, 1 Jul 2025 10:00:00 +0000\r\n\
Message-ID: <abc@example.com>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=b\r\n\
\r\n\
--b\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Hello\r\n\
--b\r\n\
Content-Type: application/pdf; name=a.pdf\r\n\
Content-Disposition: attachment; filename=a.pdf\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
JVBERi0=\r\n\
--b--\r\n";
        let msg = parse_eml(raw).unwrap();
        assert_eq!(msg.subject.as_deref(), Some("Grüße"));
        assert_eq!(
            msg.from.as_ref().unwrap().email.as_deref(),
            Some("ann@example.com")
        );
        assert_eq!(msg.recipients.len(), 3);
        assert_eq!(msg.recipients[2].kind, RecipientKind::Cc);
        assert_eq!(msg.message_id.as_deref(), Some("<abc@example.com>"));
        assert!(msg.body.as_deref().unwrap().starts_with("Hello"));
        assert_eq!(msg.attachments.len(), 1);
        assert_eq!(msg.attachments[0].filename.as_deref(), Some("a.pdf"));
        assert_eq!(msg.attachments[0].data, b"%PDF-");
        assert_eq!(msg.sent_time, Some(unix_to_filetime(1_751_364_000)));
        assert!(msg.sender.is_none(), "no Sender header");
    }

    #[test]
    fn body_without_content_type_is_plain_text() {
        let msg = parse_eml(b"From: a@x.org\nSubject: s\n\nhello\n").unwrap();
        assert_eq!(msg.body.as_deref(), Some("hello\n"));
    }

    #[test]
    fn maps_priority_and_sensitivity_headers() {
        let m = parse_eml(b"X-Priority: 1 (Highest)\r\nSensitivity: Company-Confidential\r\n\r\nx")
            .unwrap();
        assert_eq!(m.importance, Some(Importance::High));
        assert_eq!(m.sensitivity, Some(Sensitivity::Confidential));
        // Importance wins over X-Priority.
        let m = parse_eml(b"Importance: low\r\nX-Priority: 1\r\n\r\nx").unwrap();
        assert_eq!(m.importance, Some(Importance::Low));
        let m = parse_eml(b"Priority: non-urgent\r\n\r\nx").unwrap();
        assert_eq!(m.importance, Some(Importance::Low));
        assert_eq!(parse_eml(b"Subject: x\r\n\r\nx").unwrap().importance, None);
    }

    #[test]
    fn keeps_signed_entity_for_outlook() {
        let raw = b"From: a@x.org\r\nSubject: signed\r\nMIME-Version: 1.0\r\n\
Content-Type: multipart/signed; protocol=\"application/pkcs7-signature\";\r\n micalg=sha-256; boundary=\"s\"\r\n\r\n\
--s\r\nContent-Type: text/plain\r\n\r\nSigned text\r\n--s\r\n\
Content-Type: application/pkcs7-signature; name=smime.p7s\r\nContent-Transfer-Encoding: base64\r\n\r\nMIIB\r\n--s--\r\n";
        let m = parse_eml(raw).unwrap();
        assert_eq!(
            m.message_class.as_deref(),
            Some("IPM.Note.SMIME.MultipartSigned")
        );
        assert!(m.body.as_deref().unwrap().starts_with("Signed text"));
        assert_eq!(m.attachments.len(), 1);
        let p7m = &m.attachments[0];
        assert_eq!(p7m.filename.as_deref(), Some("smime.p7m"));
        // The entity re-parses as the same multipart/signed structure.
        let entity = MessageParser::new().parse(&p7m.data).unwrap();
        assert!(entity.root_part().is_content_type("multipart", "signed"));
        assert_eq!(
            entity
                .root_part()
                .content_type()
                .unwrap()
                .attribute("boundary"),
            Some("s")
        );
        assert_eq!(entity.parts().count(), 3);
        assert!(p7m.data.starts_with(b"Content-Type: multipart/signed;"));
    }

    #[test]
    fn keeps_trailing_subject_whitespace() {
        let msg = parse_eml(b"Subject: Mike S \r\n\r\nx").unwrap();
        assert_eq!(msg.subject.as_deref(), Some("Mike S "));
        let msg = parse_eml(b"Subject:   folded\r\n  line\r\n\r\nx").unwrap();
        assert_eq!(msg.subject.as_deref(), Some("folded line"));
    }

    #[test]
    fn sender_equal_to_from_is_dropped() {
        let msg = parse_eml(b"From: A <a@x.org>\r\nSender: a@X.org\r\n\r\nx").unwrap();
        assert!(msg.sender.is_none());
        let msg = parse_eml(b"Sender: s@x.org\r\n\r\nx").unwrap();
        assert_eq!(msg.from.unwrap().email.as_deref(), Some("s@x.org"));
    }
}
