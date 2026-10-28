//! Writes PST files and reads them back with Microsoft's `outlook-pst` crate, an
//! independent reader of the format.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;
use std::rc::Rc;

use eml2pst::named_props::{NamedPropertyMap, PS_PUBLIC_STRINGS, PropName};
use eml2pst::{Attachment, Mailbox, Message, PstWriter, Recipient, RecipientKind, parse_eml};
use outlook_pst::UnicodePstFile;
use outlook_pst::ltp::prop_context::PropertyValue;
use outlook_pst::ltp::table_context::{TableContext, TableRowId};
use outlook_pst::messaging::attachment::{Attachment as _, AttachmentData, UnicodeAttachment};
use outlook_pst::messaging::folder::{Folder as _, UnicodeFolder};
use outlook_pst::messaging::message::{Message as _, UnicodeMessage};
use outlook_pst::messaging::store::{Store as _, UnicodeStore};
use outlook_pst::ndb::node_id::NodeId;

fn text(v: &PropertyValue) -> String {
    match v {
        PropertyValue::Unicode(s) => s.to_string(),
        other => panic!("not a string: {other:?}"),
    }
}

/// Every row of a table as a map from property id to value.
fn rows(table: &dyn TableContext) -> Vec<(u32, BTreeMap<u16, PropertyValue>)> {
    let context = table.context();
    table
        .rows_matrix()
        .map(|row| {
            let cells = context
                .columns()
                .iter()
                .zip(row.columns(context).unwrap())
                .filter_map(|(col, value)| {
                    let value = value?;
                    Some((
                        col.prop_id(),
                        table.read_column(&value, col.prop_type()).unwrap(),
                    ))
                })
                .collect();
            (u32::from(row.id()), cells)
        })
        .collect()
}

struct Reader {
    store: Rc<UnicodeStore>,
}

impl Reader {
    fn open(path: &Path) -> Self {
        let pst = Rc::new(UnicodePstFile::open(path).expect("open PST"));
        Reader {
            store: UnicodeStore::read(pst).expect("read store"),
        }
    }

    fn folder(&self, nid: u32) -> Rc<UnicodeFolder> {
        let entry_id = self
            .store
            .properties()
            .make_entry_id(NodeId::from(nid))
            .unwrap();
        UnicodeFolder::read(self.store.clone(), &entry_id).expect("read folder")
    }

    fn message(&self, nid: u32) -> Rc<UnicodeMessage> {
        let entry_id = self
            .store
            .properties()
            .make_entry_id(NodeId::from(nid))
            .unwrap();
        UnicodeMessage::read(self.store.clone(), &entry_id, None).expect("read message")
    }

    /// Subfolders of a folder by name.
    fn subfolders(&self, nid: u32) -> BTreeMap<String, u32> {
        let folder = self.folder(nid);
        rows(folder.hierarchy_table().expect("hierarchy table").as_ref())
            .into_iter()
            .map(|(id, cells)| (text(&cells[&0x3001]), id))
            .collect()
    }

    fn ipm_subtree(&self) -> u32 {
        let entry_id = self.store.properties().ipm_sub_tree_entry_id().unwrap();
        u32::from(entry_id.node_id())
    }
}

fn write(path: &Path, build: impl FnOnce(&mut PstWriter<BufWriter<File>>)) {
    let mut pst = PstWriter::new(
        BufWriter::new(File::create(path).unwrap()),
        "Round Trip",
        [7; 16],
    );
    build(&mut pst);
    pst.finish().unwrap();
}

fn mailbox(name: &str, email: &str) -> Mailbox {
    Mailbox {
        name: Some(name.into()),
        email: Some(email.into()),
    }
}

#[test]
fn store_folders_and_messages_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rt.pst");
    let big_body = "Line of body text with ümlauts.\r\n".repeat(2000); // ~66 KB, XBLOCK subnode
    let big_attachment: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();

    write(&path, |pst| {
        let inbox = pst.add_folder(pst.root(), "Inbox");
        let sub = pst.add_folder(inbox, "Ünïcode Sub");
        pst.add_message(
            inbox,
            &Message {
                subject: Some("RE: Hello".into()),
                from: Some(mailbox("Ann", "ann@example.com")),
                recipients: vec![
                    Recipient {
                        kind: RecipientKind::To,
                        mailbox: mailbox("Bob", "bob@example.com"),
                    },
                    Recipient {
                        kind: RecipientKind::Cc,
                        mailbox: mailbox("Cy", "cy@example.com"),
                    },
                ],
                sent_time: Some(133_000_000_000_000_000),
                body: Some(big_body.clone()),
                html: Some("<p>hi</p>".into()),
                read: false,
                attachments: vec![
                    Attachment {
                        filename: Some("small.txt".into()),
                        mime_type: Some("text/plain".into()),
                        data: b"tiny".to_vec(),
                        ..Default::default()
                    },
                    Attachment {
                        filename: Some("big.bin".into()),
                        data: big_attachment.clone(),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        )
        .unwrap();
        pst.add_message(
            sub,
            &Message {
                subject: Some("In sub".into()),
                read: true,
                ..Default::default()
            },
        )
        .unwrap();
    });

    let r = Reader::open(&path);
    assert_eq!(r.store.properties().display_name().unwrap(), "Round Trip");

    let top = r.subfolders(r.ipm_subtree());
    assert_eq!(top.keys().collect::<Vec<_>>(), ["Deleted Items", "Inbox"]);
    let inbox = r.folder(top["Inbox"]);
    assert_eq!(inbox.properties().content_count().unwrap(), 1);
    assert_eq!(inbox.properties().unread_count().unwrap(), 1);
    assert!(inbox.properties().has_sub_folders().unwrap());
    assert_eq!(
        r.subfolders(top["Inbox"]).keys().collect::<Vec<_>>(),
        ["Ünïcode Sub"]
    );

    let contents = rows(inbox.contents_table().unwrap().as_ref());
    assert_eq!(contents.len(), 1);
    let (msg_nid, row) = &contents[0];
    assert_eq!(text(&row[&0x0037]), "RE: Hello");
    assert_eq!(text(&row[&0x0E04]), "Bob");

    let msg = r.message(*msg_nid);
    let props = msg.properties();
    assert_eq!(text(props.get(0x0037).unwrap()), "RE: Hello");
    assert_eq!(text(props.get(0x0E1D).unwrap()), "Hello");
    assert_eq!(text(props.get(0x1000).unwrap()), big_body);
    assert_eq!(props.message_class().unwrap(), "IPM.Note");
    assert_eq!(
        props.message_flags().unwrap() & 0x11,
        0x10,
        "unread, has attachments"
    );

    let recipients = rows(msg.recipient_table().unwrap().as_ref());
    assert_eq!(recipients.len(), 2);
    assert_eq!(text(&recipients[1].1[&0x3003]), "cy@example.com");

    let attachments = rows(msg.attachment_table().unwrap().as_ref());
    assert_eq!(attachments.len(), 2);
    let sizes: Vec<_> = attachments
        .iter()
        .map(|(id, _)| {
            let att = UnicodeAttachment::read(msg.clone(), NodeId::from(*id), None).unwrap();
            match att.data().unwrap() {
                AttachmentData::Binary(b) => b.buffer().to_vec(),
                _ => panic!("not binary"),
            }
        })
        .collect();
    assert_eq!(sizes[0], b"tiny");
    assert_eq!(sizes[1], big_attachment);
}

#[test]
fn many_folders_and_messages() {
    // Enough to need multi-level node and block B-trees, a row matrix in a
    // subnode, a multi-block table heap and a two-level row index.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("many.pst");
    const MESSAGES: usize = 1200;
    write(&path, |pst| {
        let bulk = pst.add_folder(pst.root(), "Bulk");
        for i in 0..40 {
            pst.add_folder(bulk, &format!("Folder {i:02}"));
        }
        for i in 0..MESSAGES {
            let msg = Message {
                subject: Some(format!(
                    "Message number {i} with a reasonably long subject line"
                )),
                from: Some(mailbox(
                    &format!("Sender {i}"),
                    &format!("s{i}@example.com"),
                )),
                body: Some(format!("Body {i}")),
                read: i % 3 != 0,
                ..Default::default()
            };
            pst.add_message(bulk, &msg).unwrap();
        }
    });

    let r = Reader::open(&path);
    let bulk_nid = r.subfolders(r.ipm_subtree())["Bulk"];
    assert_eq!(r.subfolders(bulk_nid).len(), 40);
    let bulk = r.folder(bulk_nid);
    assert_eq!(bulk.properties().content_count().unwrap(), MESSAGES as i32);
    assert_eq!(
        bulk.properties().unread_count().unwrap(),
        MESSAGES.div_ceil(3) as i32
    );
    let contents = rows(bulk.contents_table().unwrap().as_ref());
    assert_eq!(contents.len(), MESSAGES);
    for (i, (nid, row)) in contents.iter().enumerate() {
        assert_eq!(
            text(&row[&0x0037]),
            format!("Message number {i} with a reasonably long subject line")
        );
        assert!(
            bulk.contents_table()
                .unwrap()
                .find_row(TableRowId::new(*nid))
                .is_ok()
        );
    }
    let last = r.message(contents[MESSAGES - 1].0);
    assert_eq!(
        text(last.properties().get(0x1000).unwrap()),
        format!("Body {}", MESSAGES - 1)
    );
}

#[test]
fn eml_fixtures_convert() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("eml.pst");
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample/Inbox/html-attach.eml");
    let msg = parse_eml(&std::fs::read(fixture).unwrap()).unwrap();
    write(&path, |pst| {
        let inbox = pst.add_folder(pst.root(), "Inbox");
        pst.add_message(inbox, &msg).unwrap();
    });

    let r = Reader::open(&path);
    let inbox = r.folder(r.subfolders(r.ipm_subtree())["Inbox"]);
    let contents = rows(inbox.contents_table().unwrap().as_ref());
    let m = r.message(contents[0].0);
    let props = m.properties();
    assert_eq!(text(props.get(0x0037).unwrap()), "RE: Grüße aus München");
    assert_eq!(text(props.get(0x0C1A).unwrap()), "Jörg");
    assert_eq!(text(props.get(0x1035).unwrap()), "<html-2@example.de>");
    match props.get(0x1013).unwrap() {
        PropertyValue::Binary(b) => {
            assert!(String::from_utf8_lossy(b.buffer()).contains("<b>Grüße</b>"))
        }
        other => panic!("html not binary: {other:?}"),
    }
    assert!(text(props.get(0x007D).unwrap()).contains("Message-ID: <html-2@example.de>"));
}

#[test]
fn on_behalf_sender_subject_whitespace_invite_and_draft() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("behalf.pst");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/sample/Inbox/on-behalf-invite.eml");
    let mut msg = parse_eml(&std::fs::read(fixture).unwrap()).unwrap();
    msg.unsent = true;
    msg.categories = vec!["Work".into(), "Ünïcode label".into()];
    write(&path, |pst| {
        let drafts = pst.add_folder(pst.root(), "Drafts");
        pst.add_message(drafts, &msg).unwrap();
    });

    let r = Reader::open(&path);
    let drafts = r.folder(r.subfolders(r.ipm_subtree())["Drafts"]);
    let contents = rows(drafts.contents_table().unwrap().as_ref());
    let m = r.message(contents[0].0);
    let props = m.properties();
    assert_eq!(text(props.get(0x0037).unwrap()), "Mike S ");
    // PidTagSender* is the Sender header, PidTagSentRepresenting* is From.
    assert_eq!(
        text(props.get(0x0C1F).unwrap()),
        "jerry=datadirect.example@email.datadirect.example"
    );
    assert_eq!(text(props.get(0x0065).unwrap()), "jerry@datadirect.example");
    assert_eq!(text(props.get(0x0042).unwrap()), "Jerry");
    assert_eq!(props.message_flags().unwrap() & 0x08, 0x08, "unsent");

    // Categories live in the named property PS_PUBLIC_STRINGS "Keywords",
    // also present as a contents table column.
    let keywords =
        NamedPropertyMap::default().id(PS_PUBLIC_STRINGS, PropName::String("Keywords".into()));
    let expect = ["Work", "Ünïcode label"];
    for value in [props.get(keywords).unwrap(), &contents[0].1[&keywords]] {
        match value {
            PropertyValue::MultipleUnicode(v) => {
                assert_eq!(v.iter().map(|s| s.to_string()).collect::<Vec<_>>(), expect)
            }
            other => panic!("categories: {other:?}"),
        }
    }

    let attachments = rows(m.attachment_table().unwrap().as_ref());
    assert_eq!(text(&attachments[0].1[&0x3704]), "invite.ics");
}

/// The property ID a PST assigned to a numeric named property, read from the
/// store's own name-to-ID map.
fn named_id(r: &Reader, guid: [u8; 16], lid: u32) -> u16 {
    let map = r.store.named_property_map().unwrap();
    let props = map.properties();
    let bin = |id: u16| match props.get(id) {
        Some(PropertyValue::Binary(b)) => b.buffer().to_vec(),
        other => panic!("name map stream {id:#x}: {other:?}"),
    };
    let (guids, entries) = (bin(0x0002), bin(0x0003));
    let guid_index = 3 + guids
        .chunks(16)
        .position(|g| g == guid)
        .expect("GUID in map") as u16;
    entries
        .chunks(8)
        .find_map(|e| {
            let id = u32::from_le_bytes(e[0..4].try_into().unwrap());
            let w_guid = u16::from_le_bytes(e[4..6].try_into().unwrap());
            (id == lid && w_guid == guid_index << 1)
                .then(|| 0x8000 + u16::from_le_bytes(e[6..8].try_into().unwrap()))
        })
        .expect("named property in map")
}

#[test]
fn meeting_request_and_reply_round_trip() {
    use eml2pst::named_props::{PSETID_APPOINTMENT, PSETID_MEETING};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("meeting.pst");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/meeting");
    let request =
        parse_eml(&std::fs::read(fixtures.join("recurring-request.eml")).unwrap()).unwrap();
    let reply = parse_eml(&std::fs::read(fixtures.join("declined-reply.eml")).unwrap()).unwrap();
    assert!(
        request.attachments.is_empty(),
        "the .ics became the meeting object"
    );
    write(&path, |pst| {
        let inbox = pst.add_folder(pst.root(), "Inbox");
        pst.add_message(inbox, &request).unwrap();
        pst.add_message(inbox, &reply).unwrap();
    });

    let r = Reader::open(&path);
    let inbox = r.folder(r.subfolders(r.ipm_subtree())["Inbox"]);
    let contents = rows(inbox.contents_table().unwrap().as_ref());
    let req = r.message(contents[0].0);
    let props = req.properties();
    assert_eq!(
        props.message_class().unwrap(),
        "IPM.Schedule.Meeting.Request"
    );
    let time = |id: u16| match props.get(id) {
        Some(PropertyValue::Time(t)) => *t,
        other => panic!("{id:#x}: {other:?}"),
    };
    // 2024-10-07 09:30 in W. Europe (UTC+2 in October) is 07:30 UTC.
    let start = eml2pst::eml::unix_to_filetime(1_728_286_200);
    assert_eq!(time(named_id(&r, PSETID_APPOINTMENT, 0x820D)), start);
    assert_eq!(
        time(named_id(&r, PSETID_APPOINTMENT, 0x820E)),
        start + 30 * 60 * 10_000_000
    );
    assert_eq!(
        text(props.get(named_id(&r, PSETID_APPOINTMENT, 0x8208)).unwrap()),
        "Room 4"
    );
    assert!(matches!(
        props.get(named_id(&r, PSETID_APPOINTMENT, 0x8223)),
        Some(PropertyValue::Boolean(true))
    ));
    // Tentative for the recipient, busy as the organizer intends.
    assert!(matches!(
        props.get(named_id(&r, PSETID_APPOINTMENT, 0x8205)),
        Some(PropertyValue::Integer32(1))
    ));
    assert!(matches!(
        props.get(named_id(&r, PSETID_APPOINTMENT, 0x8224)),
        Some(PropertyValue::Integer32(2))
    ));
    let recur = match props.get(named_id(&r, PSETID_APPOINTMENT, 0x8216)) {
        Some(PropertyValue::Binary(b)) => b.buffer().to_vec(),
        other => panic!("recurrence: {other:?}"),
    };
    assert_eq!(&recur[4..8], &[0x0B, 0x20, 0x01, 0x00], "weekly");
    assert_eq!(
        u32::from_le_bytes(recur[22..26].try_into().unwrap()),
        0x0A,
        "Monday and Wednesday"
    );
    assert_eq!(
        u32::from_le_bytes(recur[26..30].try_into().unwrap()),
        0x2022,
        "ends after N"
    );
    assert_eq!(u32::from_le_bytes(recur[30..34].try_into().unwrap()), 6);
    assert_eq!(
        u32::from_le_bytes(recur[38..42].try_into().unwrap()),
        1,
        "one deleted instance"
    );
    let goid = match props.get(named_id(&r, PSETID_MEETING, 0x0003)) {
        Some(PropertyValue::Binary(b)) => b.buffer().to_vec(),
        other => panic!("goid: {other:?}"),
    };
    assert_eq!(goid.len(), 56, "decoded from the Exchange UID");

    let rep = r.message(contents[1].0);
    assert_eq!(
        rep.properties().message_class().unwrap(),
        "IPM.Schedule.Meeting.Resp.Neg"
    );
}

#[test]
fn recurrence_exceptions_round_trip() {
    use eml2pst::named_props::PSETID_APPOINTMENT;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("exceptions.pst");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/meeting/recurring-with-exceptions.eml");
    let msg = parse_eml(&std::fs::read(fixture).unwrap()).unwrap();
    // One modified occurrence; the cancelled one is only a deletion.
    assert_eq!(msg.attachments.len(), 1);
    assert!(msg.attachments[0].is_exception());
    write(&path, |pst| {
        let inbox = pst.add_folder(pst.root(), "Inbox");
        pst.add_message(inbox, &msg).unwrap();
    });

    let r = Reader::open(&path);
    let inbox = r.folder(r.subfolders(r.ipm_subtree())["Inbox"]);
    let m = r.message(rows(inbox.contents_table().unwrap().as_ref())[0].0);
    let props = m.properties();
    assert_eq!(
        props.message_flags().unwrap() & 0x10,
        0,
        "exceptions are not attachments"
    );
    let recur = match props.get(named_id(&r, PSETID_APPOINTMENT, 0x8216)) {
        Some(PropertyValue::Binary(b)) => b.buffer().to_vec(),
        other => panic!("recurrence: {other:?}"),
    };
    let u32_at = |o: usize| u32::from_le_bytes(recur[o..o + 4].try_into().unwrap());
    // Deleted: the moved occurrence's original date, the EXDATE and the
    // cancelled occurrence; modified: the moved one.
    assert_eq!(u32_at(38), 3);
    assert_eq!(u32_at(54), 1);
    let rows = rows(m.attachment_table().unwrap().as_ref());
    assert_eq!(rows.len(), 1);
    assert!(
        matches!(rows[0].1[&0x3705], PropertyValue::Integer32(5)),
        "embedded message"
    );
}

#[test]
fn bounded_rule_outlook_cannot_express_round_trips() {
    use eml2pst::named_props::PSETID_APPOINTMENT;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("covered.pst");
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/meeting/recurring-request.eml");
    let text = std::fs::read_to_string(fixture)
        .unwrap()
        .replace(
            "RRULE:FREQ=WEEKLY;COUNT=6;INTERVAL=1;BYDAY=MO,WE;WKST=MO",
            "RRULE:FREQ=MONTHLY;BYMONTHDAY=7,21;COUNT=4",
        )
        .replace(
            "EXDATE;TZID=W. Europe Standard Time:20241016T093000\r\n",
            "",
        );
    let msg = parse_eml(text.as_bytes()).unwrap();
    assert!(msg.attachments.is_empty(), "nothing lost, so no .ics kept");
    write(&path, |pst| {
        let inbox = pst.add_folder(pst.root(), "Inbox");
        pst.add_message(inbox, &msg).unwrap();
    });

    let r = Reader::open(&path);
    let inbox = r.folder(r.subfolders(r.ipm_subtree())["Inbox"]);
    let m = r.message(rows(inbox.contents_table().unwrap().as_ref())[0].0);
    let recur = match m.properties().get(named_id(&r, PSETID_APPOINTMENT, 0x8216)) {
        Some(PropertyValue::Binary(b)) => b.buffer().to_vec(),
        other => panic!("recurrence: {other:?}"),
    };
    let u32_at = |o: usize| u32::from_le_bytes(recur[o..o + 4].try_into().unwrap());
    // Oct 7, Oct 21, Nov 7, Nov 21: the gaps 14, 31, 45 have GCD 1, so every
    // day from Oct 7 to Nov 21 (46 days) with the other 42 deleted.
    assert_eq!(&recur[4..8], &[0x0A, 0x20, 0x00, 0x00], "daily");
    assert_eq!(u32_at(14), 1440, "every day");
    assert_eq!(u32_at(22), 0x2021, "ends after a date");
    assert_eq!(u32_at(26), 46, "pattern occurrences Oct 7 .. Nov 21");
    assert_eq!(u32_at(34), 42, "deleted days");
}

#[test]
fn forwards_as_eml_attaches_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("forwards.pst");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let forward =
        parse_eml(&std::fs::read(fixtures.join("sample/Inbox/forwarded.eml")).unwrap()).unwrap();
    let series =
        parse_eml(&std::fs::read(fixtures.join("meeting/recurring-with-exceptions.eml")).unwrap())
            .unwrap();
    let mut pst = PstWriter::new(
        BufWriter::new(File::create(&path).unwrap()),
        "Forwards",
        [9; 16],
    );
    pst.set_forwards_as_eml(true);
    let inbox = pst.add_folder(pst.root(), "Inbox");
    pst.add_message(inbox, &forward).unwrap();
    pst.add_message(inbox, &series).unwrap();
    pst.finish().unwrap();

    let r = Reader::open(&path);
    let inbox = r.folder(r.subfolders(r.ipm_subtree())["Inbox"]);
    let contents = rows(inbox.contents_table().unwrap().as_ref());
    let by_subject = |s: &str| {
        contents
            .iter()
            .find(|(_, row)| text(&row[&0x0037]) == s)
            .unwrap()
            .0
    };

    let fwd = r.message(by_subject("Fwd: Original report"));
    assert_eq!(
        fwd.properties().message_flags().unwrap() & 0x10,
        0x10,
        "has an attachment"
    );
    let atts = rows(fwd.attachment_table().unwrap().as_ref());
    assert!(
        matches!(atts[0].1[&0x3705], PropertyValue::Integer32(1)),
        "by value"
    );
    assert_eq!(text(&atts[0].1[&0x3704]), "Original report.eml");
    let att = UnicodeAttachment::read(fwd.clone(), NodeId::from(atts[0].0), None).unwrap();
    let data = match att.data().unwrap() {
        AttachmentData::Binary(b) => b.buffer().to_vec(),
        _ => panic!("not binary"),
    };
    let inner = parse_eml(&data).unwrap();
    assert_eq!(inner.subject.as_deref(), Some("Original report"));
    assert_eq!(inner.attachments[0].filename.as_deref(), Some("report.csv"));
    match att.properties().get(0x370E) {
        Some(PropertyValue::Unicode(m)) => assert_eq!(m.to_string(), "message/rfc822"),
        other => panic!("mime type: {other:?}"),
    }

    // Recurrence exceptions stay Outlook items.
    let ser = r.message(by_subject("Weekly sync with exceptions"));
    let atts = rows(ser.attachment_table().unwrap().as_ref());
    assert!(
        matches!(atts[0].1[&0x3705], PropertyValue::Integer32(5)),
        "embedded"
    );
}
