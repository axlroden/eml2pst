//! Lists, Tables and Properties (LTP) layer, MS-PST section 2.3: the heap-on-node,
//! BTree-on-heap, property contexts and table contexts built on top of NDB nodes.

use std::collections::BTreeMap;

use crate::ndb::{MAX_BLOCK_DATA, Nid, NodeContent, SubNode, nid_type};

/// Largest single heap allocation; bigger values go to a subnode.
pub const MAX_HEAP_ALLOC: usize = 3580;

const HN_SIGNATURE: u8 = 0xEC;
pub const CLIENT_SIG_TC: u8 = 0x7C;
pub const CLIENT_SIG_BTH: u8 = 0xB5;
pub const CLIENT_SIG_PC: u8 = 0xBC;

pub mod prop_type {
    pub const INTEGER16: u16 = 0x0002;
    pub const INTEGER32: u16 = 0x0003;
    pub const FLOATING32: u16 = 0x0004;
    pub const FLOATING64: u16 = 0x0005;
    pub const CURRENCY: u16 = 0x0006;
    pub const FLOATING_TIME: u16 = 0x0007;
    pub const ERROR_CODE: u16 = 0x000A;
    pub const BOOLEAN: u16 = 0x000B;
    pub const INTEGER64: u16 = 0x0014;
    pub const STRING8: u16 = 0x001E;
    pub const UNICODE: u16 = 0x001F;
    pub const TIME: u16 = 0x0040;
    pub const GUID: u16 = 0x0048;
    pub const OBJECT: u16 = 0x000D;
    pub const BINARY: u16 = 0x0102;
    pub const MULTIPLE: u16 = 0x1000;
    pub const MULTIPLE_INTEGER16: u16 = 0x1002;
    pub const MULTIPLE_INTEGER32: u16 = 0x1003;
    pub const MULTIPLE_INTEGER64: u16 = 0x1014;
    pub const MULTIPLE_UNICODE: u16 = 0x101F;
    pub const MULTIPLE_TIME: u16 = 0x1040;
    pub const MULTIPLE_GUID: u16 = 0x1048;
    pub const MULTIPLE_BINARY: u16 = 0x1102;
}

/// A property value, typed as stored in a PST.
#[derive(Clone, Debug, PartialEq)]
pub enum PropValue {
    Integer16(i16),
    Integer32(i32),
    Floating32(f32),
    Floating64(f64),
    /// Fixed-point with four decimal places.
    Currency(i64),
    /// OLE automation date: days since 1899-12-30.
    FloatingTime(f64),
    ErrorCode(i32),
    Boolean(bool),
    Integer64(i64),
    /// FILETIME: 100-nanosecond intervals since 1601-01-01 UTC.
    Time(i64),
    Unicode(String),
    Guid([u8; 16]),
    Binary(Vec<u8>),
    MultipleInteger16(Vec<i16>),
    MultipleInteger32(Vec<i32>),
    MultipleInteger64(Vec<i64>),
    MultipleUnicode(Vec<String>),
    MultipleTime(Vec<i64>),
    MultipleGuid(Vec<[u8; 16]>),
    MultipleBinary(Vec<Vec<u8>>),
    /// PtypObject: a subnode of the same node holding the object (an embedded
    /// message), and the object's size.
    Object {
        nid: u32,
        size: u32,
    },
}

impl PropValue {
    pub fn prop_type(&self) -> u16 {
        use prop_type as t;
        match self {
            PropValue::Integer16(_) => t::INTEGER16,
            PropValue::Integer32(_) => t::INTEGER32,
            PropValue::Floating32(_) => t::FLOATING32,
            PropValue::Floating64(_) => t::FLOATING64,
            PropValue::Currency(_) => t::CURRENCY,
            PropValue::FloatingTime(_) => t::FLOATING_TIME,
            PropValue::ErrorCode(_) => t::ERROR_CODE,
            PropValue::Boolean(_) => t::BOOLEAN,
            PropValue::Integer64(_) => t::INTEGER64,
            PropValue::Time(_) => t::TIME,
            PropValue::Unicode(_) => t::UNICODE,
            PropValue::Guid(_) => t::GUID,
            PropValue::Binary(_) => t::BINARY,
            PropValue::MultipleInteger16(_) => t::MULTIPLE_INTEGER16,
            PropValue::MultipleInteger32(_) => t::MULTIPLE_INTEGER32,
            PropValue::MultipleInteger64(_) => t::MULTIPLE_INTEGER64,
            PropValue::MultipleUnicode(_) => t::MULTIPLE_UNICODE,
            PropValue::MultipleTime(_) => t::MULTIPLE_TIME,
            PropValue::MultipleGuid(_) => t::MULTIPLE_GUID,
            PropValue::MultipleBinary(_) => t::MULTIPLE_BINARY,
            PropValue::Object { .. } => t::OBJECT,
        }
    }

    /// Fixed-size little-endian encoding, for types that have one.
    fn fixed_bytes(&self) -> Option<Vec<u8>> {
        Some(match self {
            PropValue::Integer16(v) => v.to_le_bytes().to_vec(),
            PropValue::Integer32(v) | PropValue::ErrorCode(v) => v.to_le_bytes().to_vec(),
            PropValue::Floating32(v) => v.to_le_bytes().to_vec(),
            PropValue::Floating64(v) | PropValue::FloatingTime(v) => v.to_le_bytes().to_vec(),
            PropValue::Boolean(v) => vec![u8::from(*v)],
            PropValue::Integer64(v) | PropValue::Time(v) | PropValue::Currency(v) => {
                v.to_le_bytes().to_vec()
            }
            PropValue::Object { nid, size } => [nid.to_le_bytes(), size.to_le_bytes()].concat(),
            _ => return None,
        })
    }

    /// Encoding of variable-size types (and GUIDs, which are stored out of line).
    fn variable_bytes(&self) -> Vec<u8> {
        match self {
            PropValue::Unicode(s) => s.encode_utf16().flat_map(u16::to_le_bytes).collect(),
            PropValue::Guid(g) => g.to_vec(),
            PropValue::Binary(b) => b.clone(),
            // Multi-valued fixed-size types are the values back to back.
            PropValue::MultipleInteger16(v) => v.iter().flat_map(|x| x.to_le_bytes()).collect(),
            PropValue::MultipleInteger32(v) => v.iter().flat_map(|x| x.to_le_bytes()).collect(),
            PropValue::MultipleInteger64(v) | PropValue::MultipleTime(v) => {
                v.iter().flat_map(|x| x.to_le_bytes()).collect()
            }
            PropValue::MultipleGuid(v) => v.concat(),
            // Multi-valued strings and binaries: a count, an offset per item
            // from the start of the value, then the items; strings are
            // null-terminated UTF-16LE.
            PropValue::MultipleUnicode(items) => multi_variable(
                items
                    .iter()
                    .map(|s| {
                        s.encode_utf16()
                            .chain([0])
                            .flat_map(u16::to_le_bytes)
                            .collect()
                    })
                    .collect(),
            ),
            PropValue::MultipleBinary(items) => multi_variable(items.clone()),
            _ => unreachable!("fixed-size value"),
        }
    }

    /// Approximate stored size, used for PidTagMessageSize.
    pub fn stored_size(&self) -> usize {
        self.fixed_bytes()
            .map_or_else(|| self.variable_bytes().len(), |b| b.len())
    }
}

fn multi_variable(items: Vec<Vec<u8>>) -> Vec<u8> {
    let header = 4 * (items.len() + 1);
    let mut out = (items.len() as u32).to_le_bytes().to_vec();
    let mut offset = header;
    for item in &items {
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += item.len();
    }
    for item in items {
        out.extend(item);
    }
    out
}

/// Size of a value of `ptype` in a table row (4 for HNID-referenced values).
fn cell_size(ptype: u16) -> usize {
    use prop_type as t;
    match ptype {
        t::INTEGER64 | t::TIME | t::FLOATING64 | t::CURRENCY | t::FLOATING_TIME => 8,
        t::INTEGER16 => 2,
        t::BOOLEAN => 1,
        _ => 4,
    }
}

/// Builds a heap-on-node, MS-PST 2.3.1.
pub struct HeapBuilder {
    client_sig: u8,
    root: u32,
    blocks: Vec<Vec<Vec<u8>>>,
}

impl HeapBuilder {
    pub fn new(client_sig: u8) -> Self {
        HeapBuilder {
            client_sig,
            root: 0,
            blocks: vec![Vec::new()],
        }
    }

    pub fn set_root(&mut self, hid: u32) {
        self.root = hid;
    }

    /// Size of the header at the start of heap block `index`.
    fn header_size(index: usize) -> usize {
        match index {
            0 => 12,                                 // HNHDR
            i if i >= 8 && (i - 8) % 128 == 0 => 66, // HNBITMAPHDR
            _ => 2,                                  // HNPAGEHDR
        }
    }

    /// Bytes block `index` would take with `allocs` allocations of `bytes` total.
    fn block_size(index: usize, allocs: usize, bytes: usize) -> usize {
        let map_offset = (Self::header_size(index) + bytes).next_multiple_of(2);
        map_offset + 4 + 2 * (allocs + 1)
    }

    pub fn alloc(&mut self, data: Vec<u8>) -> u32 {
        assert!(
            !data.is_empty() && data.len() <= MAX_HEAP_ALLOC,
            "bad heap allocation size {}",
            data.len()
        );
        let mut index = self.blocks.len() - 1;
        let current = &self.blocks[index];
        let used: usize = current.iter().map(Vec::len).sum();
        if current.len() >= 2047
            || Self::block_size(index, current.len() + 1, used + data.len()) > MAX_BLOCK_DATA
        {
            self.blocks.push(Vec::new());
            index += 1;
        }
        let block = &mut self.blocks[index];
        block.push(data);
        ((block.len() as u32) << 5) | ((index as u32) << 16)
    }

    pub fn build(self) -> Vec<Vec<u8>> {
        let sizes: Vec<usize> = self
            .blocks
            .iter()
            .enumerate()
            .map(|(i, allocs)| Self::block_size(i, allocs.len(), allocs.iter().map(Vec::len).sum()))
            .collect();
        let fill = |i: usize| {
            sizes
                .get(i)
                .map_or(0, |&size| fill_level(MAX_BLOCK_DATA - size))
        };

        self.blocks
            .iter()
            .enumerate()
            .map(|(i, allocs)| {
                let header = Self::header_size(i);
                let mut b = vec![0u8; header];
                let mut offsets = vec![header as u16];
                for a in allocs {
                    b.extend_from_slice(a);
                    offsets.push(b.len() as u16);
                }
                if b.len() % 2 == 1 {
                    b.push(0);
                }
                let map_offset = b.len() as u16;
                b.extend_from_slice(&(allocs.len() as u16).to_le_bytes());
                b.extend_from_slice(&0u16.to_le_bytes());
                for off in offsets {
                    b.extend_from_slice(&off.to_le_bytes());
                }

                b[0..2].copy_from_slice(&map_offset.to_le_bytes());
                if i == 0 {
                    b[2] = HN_SIGNATURE;
                    b[3] = self.client_sig;
                    b[4..8].copy_from_slice(&self.root.to_le_bytes());
                    let levels = (0..8).fold(0u32, |acc, k| acc | (u32::from(fill(k)) << (4 * k)));
                    b[8..12].copy_from_slice(&levels.to_le_bytes());
                } else if header == 66 {
                    for k in 0..128 {
                        b[2 + k / 2] |= fill(i + k) << (4 * (k % 2));
                    }
                }
                b
            })
            .collect()
    }
}

/// Heap block fill level (MS-PST 2.3.1.2) for a block with `free` bytes left.
fn fill_level(free: usize) -> u8 {
    const LIMITS: [usize; 15] = [
        3584, 2560, 2048, 1792, 1536, 1280, 1024, 768, 512, 256, 128, 64, 32, 16, 8,
    ];
    LIMITS.iter().position(|&limit| free >= limit).unwrap_or(15) as u8
}

/// Writes a BTree-on-heap (MS-PST 2.3.2) over records sorted by key and returns
/// the HID of its header.
pub fn build_bth(
    heap: &mut HeapBuilder,
    key_size: u8,
    entry_size: u8,
    records: &[(u32, Vec<u8>)],
) -> u32 {
    debug_assert!(
        records.windows(2).all(|w| w[0].0 < w[1].0),
        "BTH keys must be strictly ascending"
    );
    let key_bytes = |k: u32| k.to_le_bytes()[..key_size as usize].to_vec();

    let mut levels = 0u8;
    let mut root = 0u32;
    if !records.is_empty() {
        let per_leaf = MAX_HEAP_ALLOC / (key_size as usize + entry_size as usize);
        let mut nodes: Vec<(u32, u32)> = records
            .chunks(per_leaf)
            .map(|chunk| {
                let mut data = Vec::new();
                for (k, v) in chunk {
                    debug_assert_eq!(v.len(), entry_size as usize);
                    data.extend(key_bytes(*k));
                    data.extend_from_slice(v);
                }
                (chunk[0].0, heap.alloc(data))
            })
            .collect();
        let per_index = MAX_HEAP_ALLOC / (key_size as usize + 4);
        while nodes.len() > 1 {
            levels += 1;
            nodes = nodes
                .chunks(per_index)
                .map(|chunk| {
                    let mut data = Vec::new();
                    for (k, hid) in chunk {
                        data.extend(key_bytes(*k));
                        data.extend_from_slice(&hid.to_le_bytes());
                    }
                    (chunk[0].0, heap.alloc(data))
                })
                .collect();
        }
        root = nodes[0].1;
    }

    let mut header = vec![CLIENT_SIG_BTH, key_size, entry_size, levels];
    header.extend_from_slice(&root.to_le_bytes());
    heap.alloc(header)
}

/// Allocates subnode NIDs for out-of-line values within one node.
pub struct SubnodeIds {
    next: u32,
}

impl Default for SubnodeIds {
    fn default() -> Self {
        SubnodeIds { next: 1 }
    }
}

impl SubnodeIds {
    pub fn next(&mut self, nid_type: u8) -> Nid {
        let nid = Nid::new(nid_type, self.next);
        self.next += 1;
        nid
    }
}

/// Stores a variable-size value in the heap, or in a new subnode when it is too
/// big, and returns its HNID. Empty values use HNID 0, as Outlook does.
fn store_variable(
    heap: &mut HeapBuilder,
    subnodes: &mut Vec<SubNode>,
    ids: &mut SubnodeIds,
    data: Vec<u8>,
) -> u32 {
    if data.is_empty() {
        0
    } else if data.len() <= MAX_HEAP_ALLOC {
        heap.alloc(data)
    } else {
        let nid = ids.next(nid_type::LTP);
        subnodes.push(SubNode {
            nid,
            content: NodeContent::stream(&data),
        });
        nid.0
    }
}

/// A property context (MS-PST 2.3.3) holding `props`, plus any extra subnodes
/// the object owns (recipient and attachment tables, attachments).
pub fn build_pc(
    props: &BTreeMap<u16, PropValue>,
    extra_subnodes: Vec<SubNode>,
    ids: &mut SubnodeIds,
) -> NodeContent {
    let mut heap = HeapBuilder::new(CLIENT_SIG_PC);
    let mut subnodes = extra_subnodes;
    let records: Vec<(u32, Vec<u8>)> = props
        .iter()
        .map(|(&id, value)| {
            let hnid = match value.fixed_bytes() {
                Some(bytes) if bytes.len() <= 4 => {
                    let mut inline = [0u8; 4];
                    inline[..bytes.len()].copy_from_slice(&bytes);
                    u32::from_le_bytes(inline)
                }
                Some(bytes) => heap.alloc(bytes),
                None => store_variable(&mut heap, &mut subnodes, ids, value.variable_bytes()),
            };
            let mut entry = value.prop_type().to_le_bytes().to_vec();
            entry.extend_from_slice(&hnid.to_le_bytes());
            (u32::from(id), entry)
        })
        .collect();
    let root = build_bth(&mut heap, 2, 6, &records);
    heap.set_root(root);
    NodeContent {
        blocks: heap.build(),
        subnodes,
    }
}

/// Row ID and row version columns present in every table.
pub const TAG_LTP_ROW_ID: u32 = 0x67F2_0003;
pub const TAG_LTP_ROW_VER: u32 = 0x67F3_0003;

/// A table row: its row ID, version, and cell values keyed by property tag.
pub struct TableRow {
    pub id: u32,
    pub version: u32,
    pub cells: BTreeMap<u32, PropValue>,
}

/// A table context (MS-PST 2.3.4) with the given column tags and rows.
pub fn build_tc(columns: &[u32], rows: &[TableRow]) -> NodeContent {
    // Row layout: row ID and version first, then 8-, 4-, 2- and 1-byte cells,
    // then the cell existence bitmap.
    let mut ordered: Vec<u32> = vec![TAG_LTP_ROW_ID, TAG_LTP_ROW_VER];
    for size in [8, 4, 2, 1] {
        ordered.extend(columns.iter().filter(|&&t| {
            t != TAG_LTP_ROW_ID && t != TAG_LTP_ROW_VER && cell_size(t as u16) == size
        }));
    }
    let mut layout = BTreeMap::new(); // tag -> (offset, size, bit)
    let mut offset = 0usize;
    let mut ends = [0u16; 3];
    for (bit, &tag) in ordered.iter().enumerate() {
        let size = cell_size(tag as u16);
        layout.insert(tag, (offset, size, bit));
        offset += size;
        match size {
            8 | 4 => ends = [offset as u16; 3],
            2 => ends[1..].fill(offset as u16),
            _ => ends[2] = offset as u16,
        }
    }
    let bitmap_end = offset + ordered.len().div_ceil(8);
    let row_size = bitmap_end;

    let mut heap = HeapBuilder::new(CLIENT_SIG_TC);
    let mut subnodes = Vec::new();
    let mut ids = SubnodeIds::default();

    let mut matrix = Vec::with_capacity(rows.len() * row_size);
    for row in rows {
        let mut r = vec![0u8; row_size];
        let set = |tag: u32, bytes: &[u8], r: &mut Vec<u8>| {
            let (off, size, bit) = layout[&tag];
            r[off..off + size].copy_from_slice(&bytes[..size]);
            r[offset + bit / 8] |= 0x80 >> (bit % 8);
        };
        set(TAG_LTP_ROW_ID, &row.id.to_le_bytes(), &mut r);
        set(TAG_LTP_ROW_VER, &row.version.to_le_bytes(), &mut r);
        for (&tag, value) in &row.cells {
            assert!(
                layout.contains_key(&tag),
                "cell {tag:08X} is not a column of this table"
            );
            assert_eq!(
                value.prop_type(),
                tag as u16,
                "cell {tag:08X} has the wrong type"
            );
            let bytes = match value.fixed_bytes() {
                Some(b) => b,
                None => {
                    let data = value.variable_bytes();
                    // An empty value in a row would need HNID 0, which some readers
                    // reject; leaving the cell unset reads back the same way.
                    if data.is_empty() {
                        continue;
                    }
                    store_variable(&mut heap, &mut subnodes, &mut ids, data)
                        .to_le_bytes()
                        .to_vec()
                }
            };
            set(tag, &bytes, &mut r);
        }
        matrix.extend_from_slice(&r);
    }

    let index: Vec<(u32, Vec<u8>)> = {
        let mut idx: Vec<_> = rows
            .iter()
            .enumerate()
            .map(|(i, row)| (row.id, (i as u32).to_le_bytes().to_vec()))
            .collect();
        idx.sort_by_key(|(id, _)| *id);
        idx
    };
    let row_index = build_bth(&mut heap, 4, 4, &index);

    let rows_hnid = if matrix.is_empty() {
        0
    } else if matrix.len() <= MAX_HEAP_ALLOC {
        heap.alloc(matrix)
    } else {
        // Rows never span data blocks, so each block holds a whole number of rows.
        let per_block = MAX_BLOCK_DATA / row_size;
        let nid = ids.next(nid_type::LTP);
        subnodes.push(SubNode {
            nid,
            content: NodeContent {
                blocks: matrix
                    .chunks(per_block * row_size)
                    .map(<[u8]>::to_vec)
                    .collect(),
                subnodes: Vec::new(),
            },
        });
        nid.0
    };

    let mut info = vec![CLIENT_SIG_TC, ordered.len() as u8];
    for end in [ends[0], ends[1], ends[2], bitmap_end as u16] {
        info.extend_from_slice(&end.to_le_bytes());
    }
    info.extend_from_slice(&row_index.to_le_bytes());
    info.extend_from_slice(&rows_hnid.to_le_bytes());
    info.extend_from_slice(&0u32.to_le_bytes()); // hidIndex, deprecated
    for (&tag, &(off, size, bit)) in &layout {
        info.extend_from_slice(&tag.to_le_bytes());
        info.extend_from_slice(&(off as u16).to_le_bytes());
        info.push(size as u8);
        info.push(bit as u8);
    }
    let root = heap.alloc(info);
    heap.set_root(root);
    NodeContent {
        blocks: heap.build(),
        subnodes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_levels() {
        assert_eq!(fill_level(8000), 0);
        assert_eq!(fill_level(3000), 1);
        assert_eq!(fill_level(7), 15);
    }

    #[test]
    fn heap_spills_into_new_blocks() {
        let mut heap = HeapBuilder::new(CLIENT_SIG_PC);
        let hids: Vec<u32> = (0..5)
            .map(|_| heap.alloc(vec![1; MAX_HEAP_ALLOC]))
            .collect();
        assert_eq!(hids[0], 1 << 5);
        assert_eq!(hids[1], 2 << 5);
        assert_eq!(hids[2], (1 << 5) | (1 << 16));
        let blocks = heap.build();
        assert_eq!(blocks.len(), 3);
        assert!(blocks.iter().all(|b| b.len() <= MAX_BLOCK_DATA));
    }
}
