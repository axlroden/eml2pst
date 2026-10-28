//! Writes Outlook PST files (Unicode format, MS-PST) from email messages.

mod crc;
pub mod eml;
pub mod ical;
pub mod ltp;
pub mod meeting;
pub mod messaging;
pub mod named_props;
pub mod ndb;
pub mod recurrence;
pub mod rtf;
pub mod tnef;

pub use eml::parse_eml;
pub use messaging::{
    Attachment, FolderId, Importance, Mailbox, Message, PropKey, PstWriter, Recipient,
    RecipientKind, Sensitivity,
};
