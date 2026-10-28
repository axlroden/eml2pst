//! Lists messages of a PST through Microsoft's `outlook-pst` reader:
//! `cargo run --example read_pst -- FILE [SUBJECT-FILTER]`.
//! Prints folder path, subject, and the start of the plain text body, or with
//! READ_PST_JSON=1 one JSON object per message with Message-ID and full body.

use std::rc::Rc;

use outlook_pst::UnicodePstFile;
use outlook_pst::ltp::prop_context::PropertyValue;
use outlook_pst::ltp::table_context::TableContext;
use outlook_pst::messaging::folder::{Folder as _, UnicodeFolder};
use outlook_pst::messaging::message::{Message as _, UnicodeMessage};
use outlook_pst::messaging::store::{Store as _, UnicodeStore};
use outlook_pst::ndb::node_id::NodeId;

fn text(v: Option<&PropertyValue>) -> String {
    match v {
        Some(PropertyValue::Unicode(s)) => s.to_string(),
        _ => String::new(),
    }
}

fn column(
    table: &dyn TableContext,
    row: &outlook_pst::ltp::table_context::TableRowData,
    prop_id: u16,
) -> String {
    let context = table.context();
    let columns = row.columns(context).unwrap();
    context
        .columns()
        .iter()
        .zip(columns)
        .find(|(c, _)| c.prop_id() == prop_id)
        .and_then(|(c, v)| table.read_column(&v?, c.prop_type()).ok())
        .map_or_else(String::new, |v| text(Some(&v)))
}

fn walk(store: &Rc<UnicodeStore>, nid: u32, path: &str, filter: &str, json: bool) {
    let entry_id = store.properties().make_entry_id(NodeId::from(nid)).unwrap();
    let folder = UnicodeFolder::read(store.clone(), &entry_id).unwrap();
    if let Some(contents) = folder.contents_table() {
        for row in contents.rows_matrix() {
            let subject = column(contents.as_ref(), row, 0x0037);
            if !subject.contains(filter) {
                continue;
            }
            let entry_id = store
                .properties()
                .make_entry_id(NodeId::from(u32::from(row.id())))
                .unwrap();
            let msg = UnicodeMessage::read(store.clone(), &entry_id, None).unwrap();
            if json {
                let line = serde_json::json!({
                    "folder": path,
                    "message_id": text(msg.properties().get(0x1035)),
                    "class": text(msg.properties().get(0x001A)),
                    "body": text(msg.properties().get(0x1000)),
                });
                println!("{line}");
                continue;
            }
            let body: String = text(msg.properties().get(0x1000))
                .chars()
                .take(160)
                .collect();
            println!("{path}\t{subject}\t{body:?}");
        }
    }
    if let Some(hierarchy) = folder.hierarchy_table() {
        for row in hierarchy.rows_matrix() {
            let name = column(hierarchy.as_ref(), row, 0x3001);
            walk(
                store,
                u32::from(row.id()),
                &format!("{path}/{name}"),
                filter,
                json,
            );
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: read_pst FILE [SUBJECT-FILTER]");
    let filter = args.next().unwrap_or_default();
    // READ_PST_JSON=1 prints one JSON line per message with its full body.
    let json = std::env::var_os("READ_PST_JSON").is_some();
    let pst = Rc::new(UnicodePstFile::open(&path).expect("open PST"));
    let store = UnicodeStore::read(pst).expect("read store");
    let root = u32::from(
        store
            .properties()
            .ipm_sub_tree_entry_id()
            .unwrap()
            .node_id(),
    );
    walk(&store, root, "", &filter, json);
}
