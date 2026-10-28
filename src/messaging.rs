//! Messaging layer, MS-PST section 2.4: the message store, folders, messages,
//! recipients and attachments.
//!
//! The fixed objects of a new store (root folder, search folders, table
//! templates and so on) mirror the blank PST that Outlook itself creates.

use std::collections::BTreeMap;
use std::io::{self, Seek, Write};

use crate::ltp::{PropValue, SubnodeIds, TableRow, build_pc, build_tc, prop_type};
use crate::named_props::{Guid, NamedPropertyMap, PS_PUBLIC_STRINGS, PropName};
use crate::ndb::{HeaderInfo, NdbWriter, Nid, NodeContent, SubNode, nid_type};

const NID_MESSAGE_STORE: Nid = Nid(0x21);
const NID_NAME_TO_ID_MAP: Nid = Nid(0x61);
const NID_ROOT_FOLDER: Nid = Nid(0x122);
const NID_SEARCH_MANAGEMENT_QUEUE: Nid = Nid(0x1E1);
const NID_SEARCH_ACTIVITY_LIST: Nid = Nid(0x201);
const NID_SEARCH_DOMAIN_OBJECT: Nid = Nid(0x261);
const NID_HIERARCHY_TABLE_TEMPLATE: Nid = Nid(0x60D);
const NID_CONTENTS_TABLE_TEMPLATE: Nid = Nid(0x60E);
const NID_ASSOC_CONTENTS_TABLE_TEMPLATE: Nid = Nid(0x60F);
const NID_SEARCH_CONTENTS_TABLE_TEMPLATE: Nid = Nid(0x610);
const NID_RECEIVE_FOLDER_TABLE: Nid = Nid(0x62B);
const NID_OUTGOING_QUEUE_TABLE: Nid = Nid(0x64C);
const NID_ATTACHMENT_TABLE: Nid = Nid(0x671);
const NID_RECIPIENT_TABLE: Nid = Nid(0x692);
const NID_TOMBSTONE_TABLE_TEMPLATE: Nid = Nid(0x6B6);
const NID_LREP_DUPS_TABLE_TEMPLATE: Nid = Nid(0x6D7);
const NID_SMP_TEMPLATE: Nid = Nid(0x6F8);
const NID_SPAM_SEARCH_FOLDER: Nid = Nid(0x2223);
const NID_IPM_SUBTREE: Nid = Nid(0x8022);
const NID_SEARCH_ROOT: Nid = Nid(0x8042);
const NID_DELETED_ITEMS: Nid = Nid(0x8062);
const NID_COMMON_VIEWS: Nid = Nid(0x8082);

/// Last folder index used by the fixed folders above.
const LAST_FIXED_FOLDER_INDEX: u32 = 0x404;
/// `rgnid` default for normal messages; the first message gets the next index.
const FIRST_MESSAGE_INDEX: u32 = 0x10001;

/// Property tags (id << 16 | type) used in this module.
mod tag {
    pub const IMPORTANCE: u32 = 0x0017_0003;
    pub const MESSAGE_CLASS: u32 = 0x001A_001F;
    pub const SENSITIVITY: u32 = 0x0036_0003;
    pub const PRIORITY: u32 = 0x0026_0003;
    pub const SUBJECT: u32 = 0x0037_001F;
    pub const CLIENT_SUBMIT_TIME: u32 = 0x0039_0040;
    pub const SENT_REPRESENTING_ENTRY_ID: u32 = 0x0041_0102;
    pub const SENT_REPRESENTING_NAME: u32 = 0x0042_001F;
    pub const MESSAGE_TO_ME: u32 = 0x0057_000B;
    pub const MESSAGE_CC_ME: u32 = 0x0058_000B;
    pub const SENT_REPRESENTING_ADDRTYPE: u32 = 0x0064_001F;
    pub const SENT_REPRESENTING_EMAIL: u32 = 0x0065_001F;
    pub const CONVERSATION_TOPIC: u32 = 0x0070_001F;
    pub const CONVERSATION_INDEX: u32 = 0x0071_0102;
    pub const TRANSPORT_MESSAGE_HEADERS: u32 = 0x007D_001F;
    pub const RECIPIENT_TYPE: u32 = 0x0C15_0003;
    pub const SENDER_ENTRY_ID: u32 = 0x0C19_0102;
    pub const SENDER_NAME: u32 = 0x0C1A_001F;
    pub const SENDER_ADDRTYPE: u32 = 0x0C1E_001F;
    pub const SENDER_EMAIL: u32 = 0x0C1F_001F;
    pub const DISPLAY_BCC: u32 = 0x0E02_001F;
    pub const DISPLAY_CC: u32 = 0x0E03_001F;
    pub const DISPLAY_TO: u32 = 0x0E04_001F;
    pub const PARENT_DISPLAY: u32 = 0x0E05_001F;
    pub const MESSAGE_DELIVERY_TIME: u32 = 0x0E06_0040;
    pub const MESSAGE_FLAGS: u32 = 0x0E07_0003;
    pub const MESSAGE_SIZE: u32 = 0x0E08_0003;
    pub const RESPONSIBILITY: u32 = 0x0E0F_000B;
    pub const MESSAGE_STATUS: u32 = 0x0E17_0003;
    pub const HAS_ATTACHMENTS: u32 = 0x0E1B_000B;
    pub const NORMALIZED_SUBJECT: u32 = 0x0E1D_001F;
    pub const ATTACH_SIZE: u32 = 0x0E20_0003;
    pub const ATTACH_NUMBER: u32 = 0x0E21_0003;
    pub const SEARCH_KEY_BINARY: u32 = 0x0E2A_000B;
    pub const REPL_ITEMID: u32 = 0x0E30_0102;
    pub const REPL_CHANGENUM: u32 = 0x0E33_0014;
    pub const REPL_VERSIONHISTORY: u32 = 0x0E34_0102;
    pub const REPL_FLAGS: u32 = 0x0E38_0003;
    pub const REPL_COPIEDFROM_VERSIONHISTORY: u32 = 0x0E3C_0102;
    pub const REPL_COPIEDFROM_ITEMID: u32 = 0x0E3D_0102;
    pub const RECORD_KEY: u32 = 0x0FF9_0102;
    pub const OBJECT_TYPE: u32 = 0x0FFE_0003;
    pub const ENTRY_ID: u32 = 0x0FFF_0102;
    pub const BODY: u32 = 0x1000_001F;
    pub const RTF_COMPRESSED: u32 = 0x1009_0102;
    pub const RTF_IN_SYNC: u32 = 0x0E1F_000B;
    pub const HTML: u32 = 0x1013_0102;
    pub const INTERNET_MESSAGE_ID: u32 = 0x1035_001F;
    pub const INTERNET_REFERENCES: u32 = 0x1039_001F;
    pub const IN_REPLY_TO_ID: u32 = 0x1042_001F;
    pub const ITEM_TEMPORARY_FLAGS: u32 = 0x1097_0003;
    pub const DISPLAY_NAME: u32 = 0x3001_001F;
    pub const ADDRTYPE: u32 = 0x3002_001F;
    pub const EMAIL_ADDRESS: u32 = 0x3003_001F;
    pub const COMMENT: u32 = 0x3004_001F;
    pub const CREATION_TIME: u32 = 0x3007_0040;
    pub const LAST_MODIFICATION_TIME: u32 = 0x3008_0040;
    pub const SEARCH_KEY: u32 = 0x300B_0102;
    pub const CHANGE_KEY: u32 = 0x3013_0102;
    pub const VALID_FOLDER_MASK: u32 = 0x35DF_0003;
    pub const IPM_SUBTREE_ENTRY_ID: u32 = 0x35E0_0102;
    pub const IPM_WASTEBASKET_ENTRY_ID: u32 = 0x35E3_0102;
    pub const COMMON_VIEWS_ENTRY_ID: u32 = 0x35E6_0102;
    pub const FINDER_ENTRY_ID: u32 = 0x35E7_0102;
    pub const CONTENT_COUNT: u32 = 0x3602_0003;
    pub const CONTENT_UNREAD_COUNT: u32 = 0x3603_0003;
    pub const SUBFOLDERS: u32 = 0x360A_000B;
    pub const CONTAINER_CLASS: u32 = 0x3613_001F;
    pub const ATTACH_DATA_BINARY: u32 = 0x3701_0102;
    pub const ATTACH_EXTENSION: u32 = 0x3703_001F;
    pub const ATTACH_FILENAME: u32 = 0x3704_001F;
    pub const ATTACH_METHOD: u32 = 0x3705_0003;
    pub const ATTACH_LONG_FILENAME: u32 = 0x3707_001F;
    pub const RENDERING_POSITION: u32 = 0x370B_0003;
    pub const ATTACH_MIME_TAG: u32 = 0x370E_001F;
    pub const ATTACH_CONTENT_ID: u32 = 0x3712_001F;
    pub const DISPLAY_TYPE: u32 = 0x3900_0003;
    pub const DISPLAY_NAME_7BIT: u32 = 0x39FF_001F;
    pub const SEND_RICH_INFO: u32 = 0x3A40_000B;
    pub const INTERNET_CODEPAGE: u32 = 0x3FDE_0003;
    pub const SENDER_SMTP_ADDRESS: u32 = 0x5D01_001F;
    pub const SENT_REPRESENTING_SMTP_ADDRESS: u32 = 0x5D02_001F;
    pub const SECURE_SUBMIT_FLAGS: u32 = 0x65C6_0003;
    pub const RECEIVE_FOLDER: u32 = 0x6605_0003;
    pub const PST_LR_NO_RESTRICTIONS: u32 = 0x6633_000B;
    pub const PST_HIDDEN_COUNT: u32 = 0x6635_0003;
    pub const PST_HIDDEN_UNREAD: u32 = 0x6636_0003;
    pub const SEARCH_CRITERIA_FLAGS: u32 = 0x660B_0003;
    pub const PST_CONFIG_FLAGS: u32 = 0x66FA_0003;
    pub const PST_PASSWORD: u32 = 0x67FF_0003;
    pub const LTP_PARENT_NID: u32 = 0x67F1_0003;
    pub const ATTACHMENT_HIDDEN: u32 = 0x7FFE_000B;
}

const HIERARCHY_COLUMNS: &[u32] = &[
    tag::REPL_ITEMID,
    tag::REPL_CHANGENUM,
    tag::REPL_VERSIONHISTORY,
    tag::REPL_FLAGS,
    tag::DISPLAY_NAME,
    tag::CONTENT_COUNT,
    tag::CONTENT_UNREAD_COUNT,
    tag::SUBFOLDERS,
    tag::CONTAINER_CLASS,
    tag::PST_HIDDEN_COUNT,
    tag::PST_HIDDEN_UNREAD,
];

const CONTENTS_COLUMNS: &[u32] = &[
    tag::IMPORTANCE,
    tag::MESSAGE_CLASS,
    tag::SENSITIVITY,
    tag::SUBJECT,
    tag::CLIENT_SUBMIT_TIME,
    tag::SENT_REPRESENTING_NAME,
    tag::MESSAGE_TO_ME,
    tag::MESSAGE_CC_ME,
    tag::CONVERSATION_TOPIC,
    tag::CONVERSATION_INDEX,
    tag::DISPLAY_CC,
    tag::DISPLAY_TO,
    tag::MESSAGE_DELIVERY_TIME,
    tag::MESSAGE_FLAGS,
    tag::MESSAGE_SIZE,
    tag::MESSAGE_STATUS,
    tag::REPL_ITEMID,
    tag::REPL_CHANGENUM,
    tag::REPL_VERSIONHISTORY,
    tag::REPL_FLAGS,
    tag::REPL_COPIEDFROM_VERSIONHISTORY,
    tag::REPL_COPIEDFROM_ITEMID,
    tag::ITEM_TEMPORARY_FLAGS,
    tag::LAST_MODIFICATION_TIME,
    tag::CHANGE_KEY,
    tag::SECURE_SUBMIT_FLAGS,
];

const ASSOC_CONTENTS_COLUMNS: &[u32] = &[
    tag::MESSAGE_CLASS,
    tag::MESSAGE_FLAGS,
    tag::MESSAGE_STATUS,
    tag::DISPLAY_NAME,
    0x6800_001F, // PidTagOfflineAddressBookName
    0x6803_000B, // PidTagSendOutlookRecallReport
    0x6805_1003, // PidTagOfflineAddressBookTruncatedProperties
    0x682F_001F, // PidTagViewDescriptorName
    0x7003_0003, // PidTagViewDescriptorFlags
    0x7004_0102, // PidTagViewDescriptorLinkTo
    0x7005_0102, // PidTagViewDescriptorViewFolder
    0x7006_001F, // PidTagViewDescriptorName (string)
    0x7007_0003, // PidTagViewDescriptorVersion
];

const SEARCH_CONTENTS_COLUMNS: &[u32] = &[
    tag::IMPORTANCE,
    tag::MESSAGE_CLASS,
    tag::SENSITIVITY,
    tag::SUBJECT,
    tag::SENT_REPRESENTING_NAME,
    tag::MESSAGE_TO_ME,
    tag::MESSAGE_CC_ME,
    tag::DISPLAY_CC,
    tag::DISPLAY_TO,
    tag::PARENT_DISPLAY,
    tag::MESSAGE_DELIVERY_TIME,
    tag::MESSAGE_FLAGS,
    tag::MESSAGE_SIZE,
    tag::MESSAGE_STATUS,
    tag::SEARCH_KEY_BINARY,
    tag::LAST_MODIFICATION_TIME,
    tag::LTP_PARENT_NID,
];

const RECEIVE_FOLDER_COLUMNS: &[u32] = &[tag::MESSAGE_CLASS, tag::RECEIVE_FOLDER];

const OUTGOING_QUEUE_COLUMNS: &[u32] = &[
    tag::CLIENT_SUBMIT_TIME,
    0x0E10_0003, // PidTagSpoolerStatus
    0x0E14_0003, // PidTagSubmitFlags
];

const ATTACHMENT_COLUMNS: &[u32] = &[
    tag::ATTACH_SIZE,
    tag::ATTACH_FILENAME,
    tag::ATTACH_METHOD,
    tag::RENDERING_POSITION,
];

const RECIPIENT_COLUMNS: &[u32] = &[
    tag::RECIPIENT_TYPE,
    tag::RESPONSIBILITY,
    tag::RECORD_KEY,
    tag::OBJECT_TYPE,
    tag::ENTRY_ID,
    tag::DISPLAY_NAME,
    tag::ADDRTYPE,
    tag::EMAIL_ADDRESS,
    tag::SEARCH_KEY,
    tag::DISPLAY_TYPE,
    tag::DISPLAY_NAME_7BIT,
    tag::SEND_RICH_INFO,
];

const TOMBSTONE_COLUMNS: &[u32] = &[tag::REPL_CHANGENUM, 0x0E37_0102, tag::REPL_FLAGS];

const LREP_DUPS_COLUMNS: &[u32] = &[
    tag::MESSAGE_CLASS,
    tag::REPL_ITEMID,
    0x0E31_0102,
    tag::REPL_CHANGENUM,
    tag::REPL_VERSIONHISTORY,
    tag::REPL_FLAGS,
    0x0E3E_0102,
];

const SMP_COLUMNS: &[u32] = &[tag::REPL_CHANGENUM, tag::CREATION_TIME];

/// Provider UID of a MAPI one-off entry ID (MS-OXCDATA 2.2.5.1).
const ONE_OFF_PROVIDER_UID: [u8; 16] = [
    0x81, 0x2B, 0x1F, 0xA4, 0xBE, 0xA3, 0x10, 0x19, 0x9D, 0x6E, 0x00, 0xDD, 0x01, 0x0F, 0x54, 0x02,
];

const MSGFLAG_READ: i32 = 0x01;
const MSGFLAG_UNSENT: i32 = 0x08;
const MSGFLAG_HASATTACH: i32 = 0x10;
const ATTACH_BY_VALUE: i32 = 1;
const ATTACH_EMBEDDED_MSG: i32 = 5;
const MAPI_MAILUSER: i32 = 6;
const MAPI_ATTACH: i32 = 7;
const CODEPAGE_UTF8: i32 = 65001;

/// A folder handle returned by [`PstWriter`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FolderId(Nid);

#[derive(Clone, Debug, Default)]
pub struct Mailbox {
    pub name: Option<String>,
    pub email: Option<String>,
}

impl Mailbox {
    fn display(&self) -> String {
        self.name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .or_else(|| self.email.clone())
            .unwrap_or_default()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecipientKind {
    To = 1,
    Cc = 2,
    Bcc = 3,
}

#[derive(Clone, Debug)]
pub struct Recipient {
    pub kind: RecipientKind,
    pub mailbox: Mailbox,
}

#[derive(Clone, Debug, Default)]
pub struct Attachment {
    pub filename: Option<String>,
    pub mime_type: Option<String>,
    pub content_id: Option<String>,
    /// Referenced from the HTML body rather than listed as a file.
    pub inline: bool,
    pub data: Vec<u8>,
    /// A message attached as an Outlook item (forwarded mail, a recurrence
    /// exception) instead of `data`.
    pub embedded: Option<Box<Message>>,
    /// Further properties, set after (and overriding) the mapped ones.
    pub extra_props: Vec<(PropKey, PropValue)>,
}

impl Attachment {
    /// A recurrence exception (PidTagAttachmentFlags afException), which
    /// Outlook hides and does not count as an attachment.
    pub fn is_exception(&self) -> bool {
        self.extra_props.iter().any(|(k, v)| {
            *k == PropKey::Id(0x7FFD) && matches!(v, PropValue::Integer32(f) if f & 0x2 != 0)
        })
    }
}

/// Identifies a property: a plain property ID, or a named property that gets
/// its ID from the store's named property map.
#[derive(Clone, Debug, PartialEq)]
pub enum PropKey {
    Id(u16),
    Named(Guid, PropName),
}

/// PidTagImportance values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Importance {
    Low = 0,
    Normal = 1,
    High = 2,
}

/// PidTagSensitivity values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sensitivity {
    Normal = 0,
    Personal = 1,
    Private = 2,
    Confidential = 3,
}

/// A message in format-independent form. Times are FILETIMEs.
#[derive(Clone, Debug, Default)]
pub struct Message {
    pub message_class: Option<String>,
    pub subject: Option<String>,
    pub from: Option<Mailbox>,
    /// The Sender header, when it differs from From (mail sent on behalf of From).
    pub sender: Option<Mailbox>,
    pub recipients: Vec<Recipient>,
    pub sent_time: Option<i64>,
    pub received_time: Option<i64>,
    pub body: Option<String>,
    pub html: Option<String>,
    pub transport_headers: Option<String>,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
    pub read: bool,
    /// A draft: never sent.
    pub unsent: bool,
    /// Outlook categories (PidNameKeywords), e.g. from Gmail labels.
    pub categories: Vec<String>,
    pub importance: Option<Importance>,
    pub sensitivity: Option<Sensitivity>,
    /// Further properties, set after (and overriding) the mapped ones.
    pub extra_props: Vec<(PropKey, PropValue)>,
    pub attachments: Vec<Attachment>,
}

struct Folder {
    parent: Nid,
    name: String,
    comment: Option<String>,
    container_class: Option<String>,
    children: Vec<Nid>,
    messages: Vec<TableRow>,
}

pub struct PstWriter<W: Write + Seek> {
    ndb: NdbWriter<W>,
    record_key: [u8; 16],
    display_name: String,
    folders: BTreeMap<Nid, Folder>,
    last_folder_index: u32,
    next_message_index: u32,
    /// Counter for table row versions, stored as the header's dwUnique.
    unique: u32,
    named_props: NamedPropertyMap,
    plain_text_only: bool,
    forwards_as_eml: bool,
    /// Columns of folder contents tables: the template plus categories.
    contents_columns: Vec<u32>,
}

impl<W: Write + Seek> PstWriter<W> {
    /// Starts a new store. `record_key` identifies the store in entry IDs and
    /// should be random for each file.
    pub fn new(out: W, display_name: &str, record_key: [u8; 16]) -> Self {
        let mut folders = BTreeMap::new();
        folders.insert(
            NID_IPM_SUBTREE,
            Folder {
                parent: NID_ROOT_FOLDER,
                name: "Top of Outlook data file".into(),
                comment: None,
                container_class: None,
                children: vec![NID_DELETED_ITEMS],
                messages: Vec::new(),
            },
        );
        folders.insert(
            NID_DELETED_ITEMS,
            Folder {
                parent: NID_IPM_SUBTREE,
                name: "Deleted Items".into(),
                comment: Some("Deleted Items folder".into()),
                container_class: Some("IPF.Note".into()),
                children: Vec::new(),
                messages: Vec::new(),
            },
        );
        let mut named_props = NamedPropertyMap::default();
        let keywords = named_props.id(PS_PUBLIC_STRINGS, PropName::String("Keywords".into()));
        let mut contents_columns = CONTENTS_COLUMNS.to_vec();
        contents_columns.push((u32::from(keywords) << 16) | u32::from(prop_type::MULTIPLE_UNICODE));
        PstWriter {
            ndb: NdbWriter::new(out),
            record_key,
            display_name: display_name.into(),
            folders,
            last_folder_index: LAST_FIXED_FOLDER_INDEX,
            next_message_index: FIRST_MESSAGE_INDEX,
            unique: 0x20,
            named_props,
            plain_text_only: false,
            forwards_as_eml: false,
            contents_columns,
        }
    }

    /// Keeps plain-text messages without the HTML rendering added for New
    /// Outlook for Mac's import (see `message_props`).
    pub fn set_plain_text_only(&mut self, plain_text_only: bool) {
        self.plain_text_only = plain_text_only;
    }

    /// Attaches forwarded messages as .eml files instead of Outlook items.
    /// New Outlook for Mac's PST import does not read attached Outlook items;
    /// it keeps .eml files. Recurrence exceptions stay Outlook items.
    pub fn set_forwards_as_eml(&mut self, forwards_as_eml: bool) {
        self.forwards_as_eml = forwards_as_eml;
    }

    /// The top of the visible folder tree.
    pub fn root(&self) -> FolderId {
        FolderId(NID_IPM_SUBTREE)
    }

    pub fn deleted_items(&self) -> FolderId {
        FolderId(NID_DELETED_ITEMS)
    }

    pub fn add_folder(&mut self, parent: FolderId, name: &str) -> FolderId {
        self.last_folder_index += 1;
        let nid = Nid::new(nid_type::NORMAL_FOLDER, self.last_folder_index);
        self.folders.insert(
            nid,
            Folder {
                parent: parent.0,
                name: name.into(),
                comment: None,
                container_class: Some("IPF.Note".into()),
                children: Vec::new(),
                messages: Vec::new(),
            },
        );
        self.folders
            .get_mut(&parent.0)
            .expect("unknown parent folder")
            .children
            .push(nid);
        FolderId(nid)
    }

    /// Finds a direct subfolder by name.
    pub fn find_folder(&self, parent: FolderId, name: &str) -> Option<FolderId> {
        self.folders[&parent.0]
            .children
            .iter()
            .find(|nid| self.folders[nid].name == name)
            .map(|&nid| FolderId(nid))
    }

    fn apply(&mut self, props: &mut BTreeMap<u16, PropValue>, extra: &[(PropKey, PropValue)]) {
        for (key, value) in extra {
            let id = match key {
                PropKey::Id(id) => *id,
                PropKey::Named(guid, name) => self.named_props.id(*guid, name.clone()),
            };
            props.insert(id, value.clone());
        }
    }

    fn next_unique(&mut self) -> u32 {
        self.unique += 1;
        self.unique
    }

    /// Writes a message into `folder`. The message data goes to disk at once;
    /// only its contents table row is kept until [`Self::finish`].
    pub fn add_message(&mut self, folder: FolderId, msg: &Message) -> io::Result<()> {
        let nid = Nid::new(nid_type::NORMAL_MESSAGE, self.next_message_index);
        self.next_message_index += 1;
        let (content, props) = self.build_message(msg);
        self.ndb.add_node(nid, Some(folder.0), &content)?;

        let row = TableRow {
            id: nid.0,
            version: self.next_unique(),
            cells: self
                .contents_columns
                .iter()
                .filter_map(|t| {
                    props
                        .get(&((t >> 16) as u16))
                        .filter(|v| v.prop_type() == *t as u16)
                        .map(|v| (*t, v.clone()))
                })
                .collect(),
        };
        self.folders
            .get_mut(&folder.0)
            .expect("unknown folder")
            .messages
            .push(row);
        Ok(())
    }

    /// A message's property context with its recipient table, attachment
    /// table and attachments (and their embedded messages) as subnodes.
    fn build_message(&mut self, msg: &Message) -> (NodeContent, BTreeMap<u16, PropValue>) {
        let mut subnodes = Vec::new();
        let recipient_rows: Vec<TableRow> = msg
            .recipients
            .iter()
            .enumerate()
            .map(|(i, r)| TableRow {
                id: i as u32,
                version: self.next_unique(),
                cells: recipient_props(r),
            })
            .collect();
        subnodes.push(SubNode {
            nid: NID_RECIPIENT_TABLE,
            content: build_tc(RECIPIENT_COLUMNS, &recipient_rows),
        });

        let mut attachments_size = 0usize;
        if !msg.attachments.is_empty() {
            let mut rows = Vec::new();
            for (i, att) in msg.attachments.iter().enumerate() {
                let att_nid = Nid::new(nid_type::ATTACHMENT, i as u32 + 1);
                // With forwards_as_eml, an attached message other than a
                // recurrence exception is stored as its .eml file instead.
                let embedded = att
                    .embedded
                    .as_deref()
                    .filter(|_| !self.forwards_as_eml || att.is_exception());
                let mut props = attachment_props(att, embedded, i);
                let mut att_subnodes = Vec::new();
                if let Some(inner) = embedded {
                    let (content, inner_props) = self.build_message(inner);
                    let size = match inner_props.get(&((tag::MESSAGE_SIZE >> 16) as u16)) {
                        Some(PropValue::Integer32(n)) => *n as u32,
                        _ => 0,
                    };
                    let object = Nid::new(nid_type::NORMAL_MESSAGE, i as u32 + 1);
                    att_subnodes.push(SubNode {
                        nid: object,
                        content,
                    });
                    props.insert(
                        (tag::ATTACH_DATA_BINARY >> 16) as u16,
                        PropValue::Object {
                            nid: object.0,
                            size,
                        },
                    );
                    set(
                        &mut props,
                        tag::ATTACH_METHOD,
                        PropValue::Integer32(ATTACH_EMBEDDED_MSG),
                    );
                    set(
                        &mut props,
                        tag::ATTACH_SIZE,
                        PropValue::Integer32(size as i32),
                    );
                    attachments_size += size as usize;
                } else {
                    attachments_size += att.data.len();
                }
                self.apply(&mut props, &att.extra_props);
                rows.push(TableRow {
                    id: att_nid.0,
                    version: self.next_unique(),
                    cells: ATTACHMENT_COLUMNS
                        .iter()
                        .filter_map(|t| props.get(&((t >> 16) as u16)).map(|v| (*t, v.clone())))
                        .collect(),
                });
                subnodes.push(SubNode {
                    nid: att_nid,
                    content: build_pc(&props, att_subnodes, &mut SubnodeIds::default()),
                });
            }
            subnodes.push(SubNode {
                nid: NID_ATTACHMENT_TABLE,
                content: build_tc(ATTACHMENT_COLUMNS, &rows),
            });
        }

        let mut props = message_props(msg, self.plain_text_only);
        if !msg.categories.is_empty() {
            let keywords = self
                .named_props
                .id(PS_PUBLIC_STRINGS, PropName::String("Keywords".into()));
            props.insert(keywords, PropValue::MultipleUnicode(msg.categories.clone()));
        }
        self.apply(&mut props, &msg.extra_props);
        let size: usize =
            props.values().map(PropValue::stored_size).sum::<usize>() + attachments_size;
        set(
            &mut props,
            tag::MESSAGE_SIZE,
            PropValue::Integer32(size.min(i32::MAX as usize) as i32),
        );
        let content = build_pc(&props, subnodes, &mut SubnodeIds::default());
        (content, props)
    }

    fn entry_id(&self, nid: Nid) -> Vec<u8> {
        let mut e = vec![0u8; 4];
        e.extend_from_slice(&self.record_key);
        e.extend_from_slice(&nid.0.to_le_bytes());
        e
    }

    fn folder_summary(&self, nid: Nid) -> BTreeMap<u16, PropValue> {
        let f = &self.folders[&nid];
        let unread = f
            .messages
            .iter()
            .filter(|row| match row.cells.get(&tag::MESSAGE_FLAGS) {
                Some(PropValue::Integer32(flags)) => flags & MSGFLAG_READ == 0,
                _ => true,
            })
            .count();
        let mut p = BTreeMap::new();
        set(
            &mut p,
            tag::DISPLAY_NAME,
            PropValue::Unicode(f.name.clone()),
        );
        set(
            &mut p,
            tag::CONTENT_COUNT,
            PropValue::Integer32(f.messages.len() as i32),
        );
        set(
            &mut p,
            tag::CONTENT_UNREAD_COUNT,
            PropValue::Integer32(unread as i32),
        );
        set(
            &mut p,
            tag::SUBFOLDERS,
            PropValue::Boolean(!f.children.is_empty()),
        );
        if let Some(class) = &f.container_class {
            set(
                &mut p,
                tag::CONTAINER_CLASS,
                PropValue::Unicode(class.clone()),
            );
        }
        if let Some(comment) = &f.comment {
            set(&mut p, tag::COMMENT, PropValue::Unicode(comment.clone()));
        }
        p
    }

    fn hierarchy_row(&mut self, nid: Nid, props: &BTreeMap<u16, PropValue>) -> TableRow {
        TableRow {
            id: nid.0,
            version: self.next_unique(),
            cells: HIERARCHY_COLUMNS
                .iter()
                .filter_map(|t| props.get(&((t >> 16) as u16)).map(|v| (*t, v.clone())))
                .collect(),
        }
    }

    fn write_folder(
        &mut self,
        nid: Nid,
        parent: Nid,
        props: &BTreeMap<u16, PropValue>,
        children: &[(Nid, BTreeMap<u16, PropValue>)],
        messages: &[TableRow],
    ) -> io::Result<()> {
        self.ndb.add_node(
            nid,
            Some(parent),
            &build_pc(props, Vec::new(), &mut SubnodeIds::default()),
        )?;
        let rows: Vec<TableRow> = children
            .iter()
            .map(|(child, p)| self.hierarchy_row(*child, p))
            .collect();
        self.ndb.add_node(
            nid.with_type(nid_type::HIERARCHY_TABLE),
            None,
            &build_tc(HIERARCHY_COLUMNS, &rows),
        )?;
        self.ndb.add_node(
            nid.with_type(nid_type::CONTENTS_TABLE),
            None,
            &build_tc(&self.contents_columns, messages),
        )?;
        self.ndb.add_node(
            nid.with_type(nid_type::ASSOC_CONTENTS_TABLE),
            None,
            &build_tc(ASSOC_CONTENTS_COLUMNS, &[]),
        )
    }

    /// Writes the folders, store objects and NDB indexes, and returns the output.
    pub fn finish(mut self) -> io::Result<W> {
        let summaries: BTreeMap<Nid, BTreeMap<u16, PropValue>> = self
            .folders
            .keys()
            .map(|&nid| (nid, self.folder_summary(nid)))
            .collect();
        let nids: Vec<Nid> = self.folders.keys().copied().collect();
        for nid in nids {
            let folder = self.folders.get_mut(&nid).expect("folder");
            let messages = std::mem::take(&mut folder.messages);
            let parent = folder.parent;
            let children: Vec<_> = folder
                .children
                .iter()
                .map(|c| (*c, summaries[c].clone()))
                .collect();
            self.write_folder(nid, parent, &summaries[&nid], &children, &messages)?;
        }

        // Root folder and the folders Outlook keeps beside the IPM subtree.
        let root_props = folder_props("", true, None);
        let search_root = folder_props("Search Root", false, None);
        let common_views = folder_props("IPM_COMMON_VIEWS", false, Some(""));
        let spam = folder_props("SPAM Search Folder 2", false, None);
        let root_children = vec![
            (NID_IPM_SUBTREE, summaries[&NID_IPM_SUBTREE].clone()),
            (NID_SEARCH_ROOT, search_root.clone()),
            (NID_SPAM_SEARCH_FOLDER, spam.clone()),
            (NID_COMMON_VIEWS, common_views.clone()),
        ];
        self.write_folder(
            NID_ROOT_FOLDER,
            NID_ROOT_FOLDER,
            &root_props,
            &root_children,
            &[],
        )?;
        self.write_folder(NID_SEARCH_ROOT, NID_ROOT_FOLDER, &search_root, &[], &[])?;
        self.write_folder(NID_COMMON_VIEWS, NID_ROOT_FOLDER, &common_views, &[], &[])?;

        // The spam search folder: properties, an empty update queue, its
        // criteria and an empty search contents table.
        let ndb = &mut self.ndb;
        ndb.add_node(
            NID_SPAM_SEARCH_FOLDER,
            Some(NID_ROOT_FOLDER),
            &build_pc(&spam, Vec::new(), &mut SubnodeIds::default()),
        )?;
        ndb.add_node(
            NID_SPAM_SEARCH_FOLDER.with_type(nid_type::SEARCH_UPDATE_QUEUE),
            None,
            &NodeContent::default(),
        )?;
        let mut criteria = BTreeMap::new();
        set(
            &mut criteria,
            tag::SEARCH_CRITERIA_FLAGS,
            PropValue::Integer32(0),
        );
        ndb.add_node(
            NID_SPAM_SEARCH_FOLDER.with_type(nid_type::SEARCH_CRITERIA_OBJECT),
            None,
            &build_pc(&criteria, Vec::new(), &mut SubnodeIds::default()),
        )?;
        ndb.add_node(
            NID_SPAM_SEARCH_FOLDER.with_type(nid_type::SEARCH_CONTENTS_TABLE),
            None,
            &build_tc(SEARCH_CONTENTS_COLUMNS, &[]),
        )?;

        // Search bookkeeping nodes. The activity list names the search folders.
        ndb.add_node(NID_SEARCH_MANAGEMENT_QUEUE, None, &NodeContent::default())?;
        ndb.add_node(
            NID_SEARCH_ACTIVITY_LIST,
            None,
            &NodeContent::stream(&NID_SPAM_SEARCH_FOLDER.0.to_le_bytes()),
        )?;
        ndb.add_node(NID_SEARCH_DOMAIN_OBJECT, None, &NodeContent::default())?;

        // Table templates and store-wide tables.
        for (nid, columns) in [
            (NID_HIERARCHY_TABLE_TEMPLATE, HIERARCHY_COLUMNS),
            (NID_CONTENTS_TABLE_TEMPLATE, CONTENTS_COLUMNS),
            (NID_ASSOC_CONTENTS_TABLE_TEMPLATE, ASSOC_CONTENTS_COLUMNS),
            (NID_SEARCH_CONTENTS_TABLE_TEMPLATE, SEARCH_CONTENTS_COLUMNS),
            (NID_OUTGOING_QUEUE_TABLE, OUTGOING_QUEUE_COLUMNS),
            (NID_ATTACHMENT_TABLE, ATTACHMENT_COLUMNS),
            (NID_RECIPIENT_TABLE, RECIPIENT_COLUMNS),
            (NID_TOMBSTONE_TABLE_TEMPLATE, TOMBSTONE_COLUMNS),
            (NID_LREP_DUPS_TABLE_TEMPLATE, LREP_DUPS_COLUMNS),
            (NID_SMP_TEMPLATE, SMP_COLUMNS),
        ] {
            ndb.add_node(nid, None, &build_tc(columns, &[]))?;
        }
        let receive_row = TableRow {
            id: 1,
            version: 2,
            cells: BTreeMap::from([
                (tag::MESSAGE_CLASS, PropValue::Unicode(String::new())),
                (
                    tag::RECEIVE_FOLDER,
                    PropValue::Integer32(NID_ROOT_FOLDER.0 as i32),
                ),
            ]),
        };
        ndb.add_node(
            NID_RECEIVE_FOLDER_TABLE,
            None,
            &build_tc(RECEIVE_FOLDER_COLUMNS, &[receive_row]),
        )?;

        let name_map = self.named_props.build();
        let ndb = &mut self.ndb;
        ndb.add_node(
            NID_NAME_TO_ID_MAP,
            None,
            &build_pc(&name_map, Vec::new(), &mut SubnodeIds::default()),
        )?;

        let mut store = BTreeMap::new();
        set(
            &mut store,
            tag::RECORD_KEY,
            PropValue::Binary(self.record_key.to_vec()),
        );
        set(
            &mut store,
            tag::DISPLAY_NAME,
            PropValue::Unicode(self.display_name.clone()),
        );
        // Valid: IPM subtree, wastebasket, common views and finder.
        set(
            &mut store,
            tag::VALID_FOLDER_MASK,
            PropValue::Integer32(0xC9),
        );
        set(
            &mut store,
            tag::IPM_SUBTREE_ENTRY_ID,
            PropValue::Binary(self.entry_id(NID_IPM_SUBTREE)),
        );
        set(
            &mut store,
            tag::IPM_WASTEBASKET_ENTRY_ID,
            PropValue::Binary(self.entry_id(NID_DELETED_ITEMS)),
        );
        set(
            &mut store,
            tag::COMMON_VIEWS_ENTRY_ID,
            PropValue::Binary(self.entry_id(NID_COMMON_VIEWS)),
        );
        set(
            &mut store,
            tag::FINDER_ENTRY_ID,
            PropValue::Binary(self.entry_id(NID_SEARCH_ROOT)),
        );
        set(
            &mut store,
            tag::PST_LR_NO_RESTRICTIONS,
            PropValue::Boolean(true),
        );
        set(
            &mut store,
            tag::PST_CONFIG_FLAGS,
            PropValue::Integer32(0x000E_0011),
        );
        set(&mut store, tag::PST_PASSWORD, PropValue::Integer32(0));
        self.ndb.add_node(
            NID_MESSAGE_STORE,
            None,
            &build_pc(&store, Vec::new(), &mut SubnodeIds::default()),
        )?;

        let folder_index = self.last_folder_index;
        let info = HeaderInfo {
            nids: vec![
                (nid_type::NORMAL_FOLDER, folder_index),
                (nid_type::HIERARCHY_TABLE, folder_index),
                (nid_type::CONTENTS_TABLE, folder_index),
                (nid_type::ASSOC_CONTENTS_TABLE, folder_index),
                (nid_type::NORMAL_MESSAGE, self.next_message_index - 1),
            ],
            unique: self.unique,
        };
        self.ndb.finish(info)
    }
}

fn set(props: &mut BTreeMap<u16, PropValue>, tag: u32, value: PropValue) {
    debug_assert_eq!(
        value.prop_type(),
        tag as u16,
        "property {tag:08X} set with the wrong type"
    );
    props.insert((tag >> 16) as u16, value);
}

/// Name, address type, email, SMTP address and entry ID tags of one address.
type AddressTags = [u32; 5];

const SENDER_TAGS: AddressTags = [
    tag::SENDER_NAME,
    tag::SENDER_ADDRTYPE,
    tag::SENDER_EMAIL,
    tag::SENDER_SMTP_ADDRESS,
    tag::SENDER_ENTRY_ID,
];

const SENT_REPRESENTING_TAGS: AddressTags = [
    tag::SENT_REPRESENTING_NAME,
    tag::SENT_REPRESENTING_ADDRTYPE,
    tag::SENT_REPRESENTING_EMAIL,
    tag::SENT_REPRESENTING_SMTP_ADDRESS,
    tag::SENT_REPRESENTING_ENTRY_ID,
];

fn set_address(p: &mut BTreeMap<u16, PropValue>, mailbox: &Mailbox, tags: AddressTags) {
    let [name_tag, addrtype_tag, email_tag, smtp_tag, entry_id_tag] = tags;
    let name = mailbox.display();
    set(p, name_tag, PropValue::Unicode(name.clone()));
    if let Some(email) = mailbox.email.as_ref().filter(|e| !e.is_empty()) {
        set(p, addrtype_tag, PropValue::Unicode("SMTP".into()));
        set(p, email_tag, PropValue::Unicode(email.clone()));
        set(p, smtp_tag, PropValue::Unicode(email.clone()));
        set(
            p,
            entry_id_tag,
            PropValue::Binary(one_off_entry_id(&name, email)),
        );
    }
}

fn folder_props(name: &str, subfolders: bool, comment: Option<&str>) -> BTreeMap<u16, PropValue> {
    let mut p = BTreeMap::new();
    set(&mut p, tag::DISPLAY_NAME, PropValue::Unicode(name.into()));
    set(&mut p, tag::CONTENT_COUNT, PropValue::Integer32(0));
    set(&mut p, tag::CONTENT_UNREAD_COUNT, PropValue::Integer32(0));
    set(&mut p, tag::SUBFOLDERS, PropValue::Boolean(subfolders));
    if let Some(c) = comment {
        set(&mut p, tag::COMMENT, PropValue::Unicode(c.into()));
    }
    p
}

/// A MAPI one-off entry ID for an SMTP address (MS-OXCDATA 2.2.5.1), Unicode.
fn one_off_entry_id(name: &str, email: &str) -> Vec<u8> {
    let mut e = vec![0u8; 4];
    e.extend_from_slice(&ONE_OFF_PROVIDER_UID);
    e.extend_from_slice(&0u16.to_le_bytes()); // version
    e.extend_from_slice(&[0x01, 0x90]); // flags: Unicode, no rich info
    for s in [name, "SMTP", email] {
        e.extend(s.encode_utf16().chain([0]).flat_map(u16::to_le_bytes));
    }
    e
}

fn search_key(email: &str) -> Vec<u8> {
    let mut k = format!("SMTP:{}", email.to_uppercase()).into_bytes();
    k.push(0);
    k
}

fn recipient_props(r: &Recipient) -> BTreeMap<u32, PropValue> {
    let name = r.mailbox.display();
    let email = r.mailbox.email.clone().unwrap_or_default();
    let mut cells = BTreeMap::new();
    cells.insert(tag::RECIPIENT_TYPE, PropValue::Integer32(r.kind as i32));
    cells.insert(tag::RESPONSIBILITY, PropValue::Boolean(false));
    cells.insert(tag::OBJECT_TYPE, PropValue::Integer32(MAPI_MAILUSER));
    cells.insert(tag::DISPLAY_TYPE, PropValue::Integer32(0));
    cells.insert(tag::DISPLAY_NAME, PropValue::Unicode(name.clone()));
    cells.insert(tag::DISPLAY_NAME_7BIT, PropValue::Unicode(name.clone()));
    cells.insert(tag::SEND_RICH_INFO, PropValue::Boolean(false));
    if !email.is_empty() {
        cells.insert(tag::ADDRTYPE, PropValue::Unicode("SMTP".into()));
        cells.insert(tag::EMAIL_ADDRESS, PropValue::Unicode(email.clone()));
        cells.insert(tag::SEARCH_KEY, PropValue::Binary(search_key(&email)));
        cells.insert(
            tag::ENTRY_ID,
            PropValue::Binary(one_off_entry_id(&name, &email)),
        );
        cells.insert(
            tag::RECORD_KEY,
            PropValue::Binary(one_off_entry_id(&name, &email)),
        );
    }
    cells
}

/// Properties of an attachment; `embedded` is the message it carries as an
/// Outlook item, if it is stored as one.
fn attachment_props(
    att: &Attachment,
    embedded: Option<&Message>,
    index: usize,
) -> BTreeMap<u16, PropValue> {
    let mut p = BTreeMap::new();
    let filename = att
        .filename
        .clone()
        .unwrap_or_else(|| format!("attachment{}", index + 1));
    set(
        &mut p,
        tag::ATTACH_METHOD,
        PropValue::Integer32(ATTACH_BY_VALUE),
    );
    set(
        &mut p,
        tag::ATTACH_NUMBER,
        PropValue::Integer32(index as i32),
    );
    set(&mut p, tag::OBJECT_TYPE, PropValue::Integer32(MAPI_ATTACH));
    set(&mut p, tag::RENDERING_POSITION, PropValue::Integer32(-1));
    if let Some(inner) = embedded {
        // An attached Outlook item is shown by its subject and has no file name.
        let subject = inner.subject.clone().filter(|s| !s.is_empty());
        let name = subject.unwrap_or_else(|| "Untitled".to_owned());
        set(&mut p, tag::DISPLAY_NAME, PropValue::Unicode(name));
        if att.inline {
            set(&mut p, tag::ATTACHMENT_HIDDEN, PropValue::Boolean(true));
        }
        return p;
    }
    set(
        &mut p,
        tag::DISPLAY_NAME,
        PropValue::Unicode(filename.clone()),
    );
    set(
        &mut p,
        tag::ATTACH_FILENAME,
        PropValue::Unicode(filename.clone()),
    );
    set(
        &mut p,
        tag::ATTACH_LONG_FILENAME,
        PropValue::Unicode(filename.clone()),
    );
    if let Some(ext) = filename
        .rfind('.')
        .map(|i| &filename[i..])
        .filter(|e| e.len() > 1)
    {
        set(
            &mut p,
            tag::ATTACH_EXTENSION,
            PropValue::Unicode(ext.into()),
        );
    }
    if let Some(mime) = &att.mime_type {
        set(
            &mut p,
            tag::ATTACH_MIME_TAG,
            PropValue::Unicode(mime.clone()),
        );
    }
    if let Some(cid) = &att.content_id {
        set(
            &mut p,
            tag::ATTACH_CONTENT_ID,
            PropValue::Unicode(cid.clone()),
        );
    }
    if att.inline {
        set(&mut p, tag::ATTACHMENT_HIDDEN, PropValue::Boolean(true));
    }
    if embedded.is_none() {
        set(
            &mut p,
            tag::ATTACH_DATA_BINARY,
            PropValue::Binary(att.data.clone()),
        );
        set(
            &mut p,
            tag::ATTACH_SIZE,
            PropValue::Integer32(att.data.len().min(i32::MAX as usize) as i32),
        );
    }
    p
}

/// Plain text as HTML that displays the same: escaped, with line breaks as
/// `<br>` and runs of spaces and tabs kept.
fn plain_text_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 8 + 160);
    out.push_str("<html><head><meta http-equiv=\"Content-Type\" content=\"text/html; charset=utf-8\"></head>\r\n<body><div style=\"font-family: Calibri, Arial, sans-serif; font-size: 11pt\">\r\n");
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push_str("<br>\r\n");
        }
        let line = line.strip_suffix('\r').unwrap_or(line);
        let mut prev_space = true; // a leading space must not collapse either
        for c in line.chars() {
            match c {
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                '"' => out.push_str("&quot;"),
                '\t' => out.push_str("&nbsp;&nbsp;&nbsp;&nbsp;"),
                ' ' if prev_space => out.push_str("&nbsp;"),
                _ => out.push(c),
            }
            prev_space = c == ' ' || c == '\t';
        }
    }
    out.push_str("\r\n</div></body></html>");
    out
}

fn message_props(msg: &Message, plain_text_only: bool) -> BTreeMap<u16, PropValue> {
    let mut p = BTreeMap::new();
    let class = msg
        .message_class
        .clone()
        .unwrap_or_else(|| "IPM.Note".into());
    set(&mut p, tag::MESSAGE_CLASS, PropValue::Unicode(class));
    let mut flags = if msg.read { MSGFLAG_READ } else { 0 };
    let has_attachments = msg.attachments.iter().any(|a| !a.is_exception());
    if has_attachments {
        flags |= MSGFLAG_HASATTACH;
    }
    if msg.unsent {
        flags |= MSGFLAG_UNSENT;
    }
    set(&mut p, tag::MESSAGE_FLAGS, PropValue::Integer32(flags));
    set(&mut p, tag::MESSAGE_STATUS, PropValue::Integer32(0));
    let importance = msg.importance.unwrap_or(Importance::Normal);
    set(
        &mut p,
        tag::IMPORTANCE,
        PropValue::Integer32(importance as i32),
    );
    // PidTagPriority: urgent 1, normal 0, non-urgent -1.
    set(
        &mut p,
        tag::PRIORITY,
        PropValue::Integer32(importance as i32 - 1),
    );
    set(
        &mut p,
        tag::SENSITIVITY,
        PropValue::Integer32(msg.sensitivity.unwrap_or(Sensitivity::Normal) as i32),
    );
    set(&mut p, tag::MESSAGE_TO_ME, PropValue::Boolean(false));
    set(&mut p, tag::MESSAGE_CC_ME, PropValue::Boolean(false));
    set(
        &mut p,
        tag::HAS_ATTACHMENTS,
        PropValue::Boolean(has_attachments),
    );
    set(
        &mut p,
        tag::SEARCH_KEY,
        PropValue::Binary(uuid::Uuid::new_v4().as_bytes().to_vec()),
    );

    if let Some(subject) = &msg.subject {
        set(&mut p, tag::SUBJECT, PropValue::Unicode(subject.clone()));
        let normalized = normalized_subject(subject);
        set(
            &mut p,
            tag::NORMALIZED_SUBJECT,
            PropValue::Unicode(normalized.clone()),
        );
        set(
            &mut p,
            tag::CONVERSATION_TOPIC,
            PropValue::Unicode(normalized),
        );
    }

    let sent = msg.sent_time.or(msg.received_time);
    let received = msg.received_time.or(msg.sent_time);
    if let Some(t) = sent {
        set(&mut p, tag::CLIENT_SUBMIT_TIME, PropValue::Time(t));
    }
    if let Some(t) = received {
        set(&mut p, tag::MESSAGE_DELIVERY_TIME, PropValue::Time(t));
    }
    if let Some(t) = received {
        set(&mut p, tag::CREATION_TIME, PropValue::Time(t));
        set(&mut p, tag::LAST_MODIFICATION_TIME, PropValue::Time(t));
    }

    // PidTagSender* is who sent the message, PidTagSentRepresenting* whom it
    // was sent for; they differ only for mail sent on behalf of someone.
    if let Some(from) = &msg.from {
        let sender = msg.sender.as_ref().unwrap_or(from);
        set_address(&mut p, sender, SENDER_TAGS);
        set_address(&mut p, from, SENT_REPRESENTING_TAGS);
    }

    for (kind, t) in [
        (RecipientKind::To, tag::DISPLAY_TO),
        (RecipientKind::Cc, tag::DISPLAY_CC),
        (RecipientKind::Bcc, tag::DISPLAY_BCC),
    ] {
        let names: Vec<String> = msg
            .recipients
            .iter()
            .filter(|r| r.kind == kind)
            .map(|r| r.mailbox.display())
            .collect();
        set(&mut p, t, PropValue::Unicode(names.join("; ")));
    }

    if let Some(body) = &msg.body {
        set(&mut p, tag::BODY, PropValue::Unicode(body.clone()));
    }
    // New Outlook for Mac's PST import writes a plain-text body into its MIME
    // as raw UTF-16 labelled UTF-8, showing an empty or one-letter body; with
    // an HTML body as well it shows that instead. So plain text also gets an
    // HTML rendering of the same text, unless plain_text_only is set.
    let generated = (msg.html.is_none() && !plain_text_only)
        .then(|| msg.body.as_deref().map(plain_text_html))
        .flatten();
    if let Some(html) = msg.html.as_ref().or(generated.as_ref()) {
        set(
            &mut p,
            tag::HTML,
            PropValue::Binary(html.as_bytes().to_vec()),
        );
        // The same HTML encapsulated in compressed RTF, the body format every
        // Outlook version reads (MS-OXRTFEX, MS-OXRTFCP).
        let rtf = crate::rtf::encapsulate_html(html);
        set(
            &mut p,
            tag::RTF_COMPRESSED,
            PropValue::Binary(crate::rtf::compress(rtf.as_bytes())),
        );
        set(&mut p, tag::RTF_IN_SYNC, PropValue::Boolean(true));
        set(
            &mut p,
            tag::INTERNET_CODEPAGE,
            PropValue::Integer32(CODEPAGE_UTF8),
        );
    }
    for (value, t) in [
        (&msg.transport_headers, tag::TRANSPORT_MESSAGE_HEADERS),
        (&msg.message_id, tag::INTERNET_MESSAGE_ID),
        (&msg.in_reply_to, tag::IN_REPLY_TO_ID),
        (&msg.references, tag::INTERNET_REFERENCES),
    ] {
        if let Some(v) = value {
            set(&mut p, t, PropValue::Unicode(v.clone()));
        }
    }
    p
}

/// The subject without a reply/forward prefix such as "RE: " (MS-OXCMSG 2.2.1.9).
fn normalized_subject(subject: &str) -> String {
    if let Some(colon) = subject.find(':') {
        let prefix = &subject[..colon];
        if !prefix.is_empty()
            && prefix.chars().count() <= 3
            && prefix.chars().all(char::is_alphanumeric)
            && !prefix.chars().all(|c| c.is_ascii_digit())
        {
            return subject[colon + 1..].trim_start().to_string();
        }
    }
    subject.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_as_html() {
        let html = plain_text_html("a <b> & \"c\"\r\n  two  spaces\tx");
        assert!(html.contains("a &lt;b&gt; &amp; &quot;c&quot;<br>\r\n&nbsp;&nbsp;two &nbsp;spaces&nbsp;&nbsp;&nbsp;&nbsp;x"));
    }

    #[test]
    fn normalizes_reply_prefixes() {
        assert_eq!(normalized_subject("RE: hello"), "hello");
        assert_eq!(normalized_subject("Fwd: x"), "x");
        assert_eq!(normalized_subject("Meeting: 10:00"), "Meeting: 10:00");
        assert_eq!(normalized_subject("12: x"), "12: x");
        assert_eq!(normalized_subject("plain"), "plain");
    }
}
