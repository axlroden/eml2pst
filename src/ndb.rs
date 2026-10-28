//! Node Database (NDB) layer of a Unicode PST file, MS-PST section 2.2.
//!
//! The writer only ever creates a new file. Data blocks are appended as they are
//! produced, and everything that indexes them (the node and block B-trees, the
//! allocation maps and the header) is built once in [`NdbWriter::finish`]. This
//! avoids free-space management and the transactional rules that editing an
//! existing PST would need.

use std::io::{self, Seek, SeekFrom, Write};

use crate::crc::crc32;

pub const PAGE_SIZE: u64 = 512;
pub const MAX_BLOCK_SIZE: usize = 8192;
pub const BLOCK_TRAILER_SIZE: usize = 16;
/// Largest payload of a single data block.
pub const MAX_BLOCK_DATA: usize = MAX_BLOCK_SIZE - BLOCK_TRAILER_SIZE;

const HEADER_SIZE: usize = 564;
const DENSITY_LIST_OFFSET: u64 = 0x4200;
const AMAP_FIRST_OFFSET: u64 = 0x4400;
/// Bytes covered by one AMap page: 496 bytes of bits, one bit per 64 bytes.
const AMAP_DATA_SIZE: u64 = 496 * 8 * 64;
const FMAP_FIRST_AMAP: u64 = 128;
const FMAP_EVERY: u64 = 496;
const FPMAP_FIRST_AMAP: u64 = 128 * 64;
const FPMAP_EVERY: u64 = 496 * 64;

const PTYPE_BBT: u8 = 0x80;
const PTYPE_NBT: u8 = 0x81;
const PTYPE_FMAP: u8 = 0x82;
const PTYPE_PMAP: u8 = 0x83;
const PTYPE_AMAP: u8 = 0x84;
const PTYPE_FPMAP: u8 = 0x85;
const PTYPE_DL: u8 = 0x86;

const BT_ENTRIES_SIZE: usize = 488;
const NBT_LEAF_ENTRY_SIZE: usize = 32;
const BBT_LEAF_ENTRY_SIZE: usize = 24;
const BT_INTERMEDIATE_ENTRY_SIZE: usize = 24;

/// Entries per XBLOCK/XXBLOCK: (8176 - 8 byte header) / 8 byte BID.
const XBLOCK_MAX_ENTRIES: usize = (MAX_BLOCK_DATA - 8) / 8;
/// Entries per SLBLOCK: (8176 - 8) / 24 byte SLENTRY.
const SLBLOCK_MAX_ENTRIES: usize = (MAX_BLOCK_DATA - 8) / 24;
/// Entries per SIBLOCK: (8176 - 8) / 16 byte SIENTRY.
const SIBLOCK_MAX_ENTRIES: usize = (MAX_BLOCK_DATA - 8) / 16;

/// Default `rgnid` values (last used index per NID type) of a fresh store.
const DEFAULT_NIDS: [u32; 32] = {
    let mut nids = [0x400u32; 32];
    nids[3] = 0x4000;
    nids[4] = 0x10000;
    nids[8] = 0x8000;
    nids
};

/// Node ID: a 5-bit type and a 27-bit index (MS-PST 2.2.2.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Nid(pub u32);

impl Nid {
    pub const fn new(nid_type: u8, index: u32) -> Self {
        Nid((index << 5) | nid_type as u32)
    }

    pub const fn nid_type(self) -> u8 {
        (self.0 & 0x1F) as u8
    }

    pub const fn index(self) -> u32 {
        self.0 >> 5
    }

    /// The node of another type that shares this node's index, such as a
    /// folder's hierarchy table.
    pub const fn with_type(self, nid_type: u8) -> Self {
        Nid::new(nid_type, self.index())
    }
}

pub mod nid_type {
    pub const INTERNAL: u8 = 0x01;
    pub const NORMAL_FOLDER: u8 = 0x02;
    pub const SEARCH_FOLDER: u8 = 0x03;
    pub const NORMAL_MESSAGE: u8 = 0x04;
    pub const ATTACHMENT: u8 = 0x05;
    pub const SEARCH_UPDATE_QUEUE: u8 = 0x06;
    pub const SEARCH_CRITERIA_OBJECT: u8 = 0x07;
    pub const RECEIVE_FOLDER_TABLE: u8 = 0x0B;
    pub const OUTGOING_QUEUE_TABLE: u8 = 0x0C;
    pub const HIERARCHY_TABLE: u8 = 0x0D;
    pub const CONTENTS_TABLE: u8 = 0x0E;
    pub const ASSOC_CONTENTS_TABLE: u8 = 0x0F;
    pub const SEARCH_CONTENTS_TABLE: u8 = 0x10;
    pub const ATTACHMENT_TABLE: u8 = 0x11;
    pub const RECIPIENT_TABLE: u8 = 0x12;
    pub const LTP: u8 = 0x1F;
}

/// Block ID. Bit 1 marks internal (XBLOCK/SLBLOCK) blocks; bit 0 is reserved.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Bid(pub u64);

impl Bid {
    pub const NONE: Bid = Bid(0);
}

/// Encoding applied to the payload of external data blocks (`bCryptMethod`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CryptMethod {
    None = 0x00,
}

/// The content of a node: its data blocks and its subnodes.
///
/// Heap-on-node data is a list of separately built heap pages, so the blocks are
/// kept as produced instead of being re-split from one byte stream.
#[derive(Clone, Debug, Default)]
pub struct NodeContent {
    pub blocks: Vec<Vec<u8>>,
    pub subnodes: Vec<SubNode>,
}

#[derive(Clone, Debug)]
pub struct SubNode {
    pub nid: Nid,
    pub content: NodeContent,
}

impl NodeContent {
    /// Content holding a plain byte stream, split into maximal data blocks.
    pub fn stream(data: &[u8]) -> Self {
        NodeContent {
            blocks: data.chunks(MAX_BLOCK_DATA).map(<[u8]>::to_vec).collect(),
            subnodes: Vec::new(),
        }
    }
}

struct BbtEntry {
    bid: Bid,
    ib: u64,
    cb: u16,
}

struct NbtEntry {
    nid: Nid,
    data: Bid,
    sub: Bid,
    parent: Nid,
}

/// Fields of the header that belong to the layers above the NDB.
pub struct HeaderInfo {
    /// Last used index per NID type, overriding the defaults where set.
    pub nids: Vec<(u8, u32)>,
    pub unique: u32,
}

pub struct NdbWriter<W: Write + Seek> {
    out: W,
    /// Next unallocated byte; always 64-byte aligned.
    pos: u64,
    next_block: u64,
    next_page: u64,
    crypt: CryptMethod,
    bbt: Vec<BbtEntry>,
    nbt: Vec<NbtEntry>,
    /// One allocation bitmap per AMap region, one bit per 64 bytes, MSB first.
    amaps: Vec<[u8; 496]>,
}

impl<W: Write + Seek> NdbWriter<W> {
    pub fn new(out: W) -> Self {
        NdbWriter {
            out,
            pos: AMAP_FIRST_OFFSET,
            next_block: 1,
            next_page: 1,
            crypt: CryptMethod::None,
            bbt: Vec::new(),
            nbt: Vec::new(),
            amaps: Vec::new(),
        }
    }

    /// Writes a node and its subnode tree, returning nothing; the node becomes
    /// part of the node B-tree written by [`Self::finish`].
    pub fn add_node(
        &mut self,
        nid: Nid,
        parent: Option<Nid>,
        content: &NodeContent,
    ) -> io::Result<()> {
        let (data, sub) = self.write_content(content)?;
        self.nbt.push(NbtEntry {
            nid,
            data,
            sub,
            parent: parent.unwrap_or(Nid(0)),
        });
        Ok(())
    }

    fn write_content(&mut self, content: &NodeContent) -> io::Result<(Bid, Bid)> {
        let sub = self.write_subnode_tree(&content.subnodes)?;
        let data = self.write_data_tree(&content.blocks)?;
        Ok((data, sub))
    }

    fn write_data_tree(&mut self, blocks: &[Vec<u8>]) -> io::Result<Bid> {
        match blocks {
            [] => Ok(Bid::NONE),
            [block] => self.write_block(block, false),
            _ => {
                let total: usize = blocks.iter().map(Vec::len).sum();
                let mut leaves = Vec::with_capacity(blocks.len());
                for block in blocks {
                    leaves.push((self.write_block(block, false)?, block.len()));
                }
                let level1 = self.write_xblocks(1, &leaves)?;
                match level1.as_slice() {
                    [(bid, _)] => Ok(*bid),
                    _ if level1.len() <= XBLOCK_MAX_ENTRIES => {
                        let level2 = self.write_xblocks(2, &level1)?;
                        Ok(level2[0].0)
                    }
                    _ => Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("node data of {total} bytes exceeds the PST data tree limit"),
                    )),
                }
            }
        }
    }

    /// Writes XBLOCKs (level 1) or XXBLOCKs (level 2) over `children`, which
    /// are (bid, byte count below it) pairs.
    fn write_xblocks(
        &mut self,
        level: u8,
        children: &[(Bid, usize)],
    ) -> io::Result<Vec<(Bid, usize)>> {
        let mut out = Vec::new();
        for chunk in children.chunks(XBLOCK_MAX_ENTRIES) {
            let total: usize = chunk.iter().map(|(_, n)| n).sum();
            let mut data = Vec::with_capacity(8 + chunk.len() * 8);
            data.push(0x01);
            data.push(level);
            data.extend_from_slice(&(chunk.len() as u16).to_le_bytes());
            data.extend_from_slice(&(total as u32).to_le_bytes());
            for (bid, _) in chunk {
                data.extend_from_slice(&bid.0.to_le_bytes());
            }
            out.push((self.write_block(&data, true)?, total));
        }
        Ok(out)
    }

    fn write_subnode_tree(&mut self, subnodes: &[SubNode]) -> io::Result<Bid> {
        if subnodes.is_empty() {
            return Ok(Bid::NONE);
        }
        let mut entries = Vec::with_capacity(subnodes.len());
        for subnode in subnodes {
            let (data, sub) = self.write_content(&subnode.content)?;
            entries.push((subnode.nid, data, sub));
        }
        entries.sort_by_key(|(nid, _, _)| *nid);
        if entries.windows(2).any(|w| w[0].0 == w[1].0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "duplicate subnode NID",
            ));
        }

        let mut leaves = Vec::new();
        for chunk in entries.chunks(SLBLOCK_MAX_ENTRIES) {
            let mut data = Vec::with_capacity(8 + chunk.len() * 24);
            data.extend_from_slice(&[0x02, 0x00]);
            data.extend_from_slice(&(chunk.len() as u16).to_le_bytes());
            data.extend_from_slice(&0u32.to_le_bytes());
            for (nid, bid_data, bid_sub) in chunk {
                data.extend_from_slice(&u64::from(nid.0).to_le_bytes());
                data.extend_from_slice(&bid_data.0.to_le_bytes());
                data.extend_from_slice(&bid_sub.0.to_le_bytes());
            }
            leaves.push((chunk[0].0, self.write_block(&data, true)?));
        }
        if leaves.len() == 1 {
            return Ok(leaves[0].1);
        }
        if leaves.len() > SIBLOCK_MAX_ENTRIES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "too many subnodes",
            ));
        }
        let mut data = Vec::with_capacity(8 + leaves.len() * 16);
        data.extend_from_slice(&[0x02, 0x01]);
        data.extend_from_slice(&(leaves.len() as u16).to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        for (nid, bid) in &leaves {
            data.extend_from_slice(&u64::from(nid.0).to_le_bytes());
            data.extend_from_slice(&bid.0.to_le_bytes());
        }
        self.write_block(&data, true)
    }

    fn write_block(&mut self, data: &[u8], internal: bool) -> io::Result<Bid> {
        assert!(data.len() <= MAX_BLOCK_DATA, "block payload too large");
        let bid = Bid((self.next_block << 2) | if internal { 0x2 } else { 0 });
        self.next_block += 1;
        let size = (data.len() + BLOCK_TRAILER_SIZE).next_multiple_of(64);
        let ib = self.alloc(size as u64, 64);

        let mut buf = vec![0u8; size];
        buf[..data.len()].copy_from_slice(data);
        let trailer = &mut buf[size - BLOCK_TRAILER_SIZE..];
        trailer[0..2].copy_from_slice(&(data.len() as u16).to_le_bytes());
        trailer[2..4].copy_from_slice(&compute_sig(ib, bid.0).to_le_bytes());
        trailer[4..8].copy_from_slice(&crc32(data).to_le_bytes());
        trailer[8..16].copy_from_slice(&bid.0.to_le_bytes());
        self.write_at(ib, &buf)?;

        self.bbt.push(BbtEntry {
            bid,
            ib,
            cb: data.len() as u16,
        });
        Ok(bid)
    }

    /// Allocates `size` bytes at the given alignment, skipping the map pages
    /// reserved at the start of every AMap region and never straddling the end
    /// of a region.
    fn alloc(&mut self, size: u64, align: u64) -> u64 {
        loop {
            let ib = self.pos.next_multiple_of(align);
            let region = (ib - AMAP_FIRST_OFFSET) / AMAP_DATA_SIZE;
            let region_start = AMAP_FIRST_OFFSET + region * AMAP_DATA_SIZE;
            let reserved_end = region_start + reserved_pages(region) * PAGE_SIZE;
            if ib < reserved_end {
                self.pos = reserved_end;
                continue;
            }
            if ib + size > region_start + AMAP_DATA_SIZE {
                self.pos = region_start + AMAP_DATA_SIZE;
                continue;
            }
            self.pos = ib + size;
            self.mark(ib, size);
            return ib;
        }
    }

    fn mark(&mut self, ib: u64, size: u64) {
        for slot in (ib..ib + size).step_by(64) {
            let region = ((slot - AMAP_FIRST_OFFSET) / AMAP_DATA_SIZE) as usize;
            while self.amaps.len() <= region {
                let mut bits = [0u8; 496];
                // The AMap page itself and any PMap/FMap/FPMap pages that follow it.
                let reserved = reserved_pages(self.amaps.len() as u64) * PAGE_SIZE / 64;
                for k in 0..reserved as usize {
                    bits[k / 8] |= 0x80 >> (k % 8);
                }
                self.amaps.push(bits);
            }
            let k = (((slot - AMAP_FIRST_OFFSET) % AMAP_DATA_SIZE) / 64) as usize;
            self.amaps[region][k / 8] |= 0x80 >> (k % 8);
        }
    }

    fn write_at(&mut self, ib: u64, data: &[u8]) -> io::Result<()> {
        self.out.seek(SeekFrom::Start(ib))?;
        self.out.write_all(data)
    }

    fn next_page_id(&mut self) -> u64 {
        let id = self.next_page;
        self.next_page += 1;
        id
    }

    fn write_page(
        &mut self,
        ib: u64,
        content: &[u8; 496],
        ptype: u8,
        bid: u64,
        signed: bool,
    ) -> io::Result<()> {
        let mut page = [0u8; PAGE_SIZE as usize];
        page[..496].copy_from_slice(content);
        page[496] = ptype;
        page[497] = ptype;
        let sig = if signed { compute_sig(ib, bid) } else { 0 };
        page[498..500].copy_from_slice(&sig.to_le_bytes());
        page[500..504].copy_from_slice(&crc32(content).to_le_bytes());
        page[504..512].copy_from_slice(&bid.to_le_bytes());
        self.write_at(ib, &page)
    }

    /// Writes one level of B-tree pages over `entries` (key, serialized entry)
    /// and returns the (first key, bid, ib) of each page.
    fn write_btree_level(
        &mut self,
        ptype: u8,
        level: u8,
        entry_size: usize,
        entries: &[(u64, Vec<u8>)],
    ) -> io::Result<Vec<(u64, u64, u64)>> {
        let max = BT_ENTRIES_SIZE / entry_size;
        let mut pages = Vec::new();
        for chunk in entries.chunks(max) {
            let mut content = [0u8; 496];
            for (i, (_, entry)) in chunk.iter().enumerate() {
                content[i * entry_size..(i + 1) * entry_size].copy_from_slice(entry);
            }
            content[488] = chunk.len() as u8;
            content[489] = max as u8;
            content[490] = entry_size as u8;
            content[491] = level;
            let bid = self.next_page_id();
            let ib = self.alloc(PAGE_SIZE, PAGE_SIZE);
            self.write_page(ib, &content, ptype, bid, true)?;
            pages.push((chunk[0].0, bid, ib));
        }
        Ok(pages)
    }

    fn write_btree(
        &mut self,
        ptype: u8,
        leaf_entry_size: usize,
        leaves: Vec<(u64, Vec<u8>)>,
    ) -> io::Result<(u64, u64)> {
        let mut level = 0;
        let mut pages = self.write_btree_level(ptype, level, leaf_entry_size, &leaves)?;
        while pages.len() > 1 {
            level += 1;
            let entries: Vec<_> = pages
                .iter()
                .map(|&(key, bid, ib)| {
                    let mut e = Vec::with_capacity(BT_INTERMEDIATE_ENTRY_SIZE);
                    e.extend_from_slice(&key.to_le_bytes());
                    e.extend_from_slice(&bid.to_le_bytes());
                    e.extend_from_slice(&ib.to_le_bytes());
                    (key, e)
                })
                .collect();
            pages = self.write_btree_level(ptype, level, BT_INTERMEDIATE_ENTRY_SIZE, &entries)?;
        }
        Ok((pages[0].1, pages[0].2))
    }

    /// Writes the B-trees, allocation maps, density list and header.
    pub fn finish(mut self, info: HeaderInfo) -> io::Result<W> {
        self.nbt.sort_by_key(|e| e.nid);
        if self.nbt.windows(2).any(|w| w[0].nid == w[1].nid) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "duplicate NID"));
        }
        let nbt_leaves: Vec<_> = self
            .nbt
            .iter()
            .map(|e| {
                let mut b = Vec::with_capacity(NBT_LEAF_ENTRY_SIZE);
                b.extend_from_slice(&u64::from(e.nid.0).to_le_bytes());
                b.extend_from_slice(&e.data.0.to_le_bytes());
                b.extend_from_slice(&e.sub.0.to_le_bytes());
                b.extend_from_slice(&e.parent.0.to_le_bytes());
                b.extend_from_slice(&0u32.to_le_bytes());
                (u64::from(e.nid.0), b)
            })
            .collect();
        let bbt_leaves: Vec<_> = self
            .bbt
            .iter()
            .map(|e| {
                let mut b = Vec::with_capacity(BBT_LEAF_ENTRY_SIZE);
                b.extend_from_slice(&e.bid.0.to_le_bytes());
                b.extend_from_slice(&e.ib.to_le_bytes());
                b.extend_from_slice(&e.cb.to_le_bytes());
                // Outlook writes a reference count of 2 for a block used once.
                b.extend_from_slice(&2u16.to_le_bytes());
                b.extend_from_slice(&0u32.to_le_bytes());
                (e.bid.0, b)
            })
            .collect();
        let nbt_root = self.write_btree(PTYPE_NBT, NBT_LEAF_ENTRY_SIZE, nbt_leaves)?;
        let bbt_root = self.write_btree(PTYPE_BBT, BBT_LEAF_ENTRY_SIZE, bbt_leaves)?;

        // The file always ends at the end of its last AMap region, as Outlook
        // grows files one region at a time.
        let regions = self.amaps.len() as u64;
        let eof = AMAP_FIRST_OFFSET + regions * AMAP_DATA_SIZE;
        let mut free_bytes = 0u64;
        for region in 0..regions {
            let ib = AMAP_FIRST_OFFSET + region * AMAP_DATA_SIZE;
            let bits = self.amaps[region as usize];
            free_bytes += bits.iter().map(|b| u64::from(b.count_zeros())).sum::<u64>() * 64;
            self.write_page(ib, &bits, PTYPE_AMAP, ib, false)?;
            let mut next = ib + PAGE_SIZE;
            if has_pmap(region) {
                // The PMap is deprecated; it is written fully allocated.
                self.write_page(next, &[0xFF; 496], PTYPE_PMAP, next, false)?;
                next += PAGE_SIZE;
            }
            if has_fmap(region) {
                let mut fmap = [0u8; 496];
                for (i, entry) in fmap.iter_mut().enumerate() {
                    if let Some(amap) = self.amaps.get((region + i as u64) as usize) {
                        *entry = max_free_run(amap);
                    }
                }
                self.write_page(next, &fmap, PTYPE_FMAP, next, false)?;
                next += PAGE_SIZE;
            }
            if has_fpmap(region) {
                self.write_page(next, &[0xFF; 496], PTYPE_FPMAP, next, false)?;
            }
        }

        // An empty density list: no entries, backfill not complete.
        let dl_bid = self.next_page_id();
        self.write_page(DENSITY_LIST_OFFSET, &[0u8; 496], PTYPE_DL, dl_bid, true)?;

        let mut nids = DEFAULT_NIDS;
        for &(nid_type, index) in &info.nids {
            nids[nid_type as usize] = index;
        }
        let header = build_header(&HeaderFields {
            next_page: self.next_page,
            unique: info.unique,
            nids,
            eof,
            amap_last: AMAP_FIRST_OFFSET + (regions - 1) * AMAP_DATA_SIZE,
            amap_free: free_bytes,
            nbt_root,
            bbt_root,
            crypt: self.crypt,
            next_block: self.next_block << 2,
        });
        self.write_at(0, &header)?;

        // Extend the file to its full size; the gaps were never written.
        self.out.seek(SeekFrom::Start(eof - 1))?;
        self.out.write_all(&[0])?;
        self.out.flush()?;
        Ok(self.out)
    }
}

struct HeaderFields {
    next_page: u64,
    unique: u32,
    nids: [u32; 32],
    eof: u64,
    amap_last: u64,
    amap_free: u64,
    nbt_root: (u64, u64),
    bbt_root: (u64, u64),
    crypt: CryptMethod,
    next_block: u64,
}

fn build_header(h: &HeaderFields) -> [u8; HEADER_SIZE] {
    let mut b = [0u8; HEADER_SIZE];
    b[0..4].copy_from_slice(b"!BDN");
    b[8..10].copy_from_slice(b"SM");
    b[10..12].copy_from_slice(&23u16.to_le_bytes()); // wVer: Unicode
    b[12..14].copy_from_slice(&19u16.to_le_bytes()); // wVerClient
    b[14] = 0x01; // bPlatformCreate
    b[15] = 0x01; // bPlatformAccess
    b[32..40].copy_from_slice(&h.next_page.to_le_bytes());
    b[40..44].copy_from_slice(&h.unique.to_le_bytes());
    for (i, nid) in h.nids.iter().enumerate() {
        b[44 + i * 4..48 + i * 4].copy_from_slice(&nid.to_le_bytes());
    }
    // ROOT at 180: dwReserved, then the fields below.
    b[184..192].copy_from_slice(&h.eof.to_le_bytes());
    b[192..200].copy_from_slice(&h.amap_last.to_le_bytes());
    b[200..208].copy_from_slice(&h.amap_free.to_le_bytes());
    // cbPMapFree stays 0: the PMap is written fully allocated.
    b[216..224].copy_from_slice(&h.nbt_root.0.to_le_bytes());
    b[224..232].copy_from_slice(&h.nbt_root.1.to_le_bytes());
    b[232..240].copy_from_slice(&h.bbt_root.0.to_le_bytes());
    b[240..248].copy_from_slice(&h.bbt_root.1.to_le_bytes());
    b[248] = 0x02; // fAMapValid: VALID_AMAP2
    // rgbFM and rgbFP are deprecated and filled with 0xFF, as Outlook does.
    b[256..512].fill(0xFF);
    b[512] = 0x80; // bSentinel
    b[513] = h.crypt as u8;
    b[516..524].copy_from_slice(&h.next_block.to_le_bytes());
    let crc_partial = crc32(&b[8..8 + 471]);
    b[4..8].copy_from_slice(&crc_partial.to_le_bytes());
    let crc_full = crc32(&b[8..524]);
    b[524..528].copy_from_slice(&crc_full.to_le_bytes());
    b
}

fn compute_sig(ib: u64, bid: u64) -> u16 {
    let v = (ib ^ bid) as u32;
    ((v >> 16) ^ v) as u16
}

fn has_pmap(region: u64) -> bool {
    region.is_multiple_of(8)
}

fn has_fmap(region: u64) -> bool {
    region >= FMAP_FIRST_AMAP && (region - FMAP_FIRST_AMAP).is_multiple_of(FMAP_EVERY)
}

fn has_fpmap(region: u64) -> bool {
    region >= FPMAP_FIRST_AMAP && (region - FPMAP_FIRST_AMAP).is_multiple_of(FPMAP_EVERY)
}

fn reserved_pages(region: u64) -> u64 {
    1 + u64::from(has_pmap(region)) + u64::from(has_fmap(region)) + u64::from(has_fpmap(region))
}

/// Longest run of free 64-byte slots in an AMap, capped at 255.
fn max_free_run(bits: &[u8; 496]) -> u8 {
    let mut best = 0u32;
    let mut run = 0u32;
    for k in 0..496 * 8 {
        if bits[k / 8] & (0x80 >> (k % 8)) == 0 {
            run += 1;
            best = best.max(run);
        } else {
            run = 0;
        }
    }
    best.min(255) as u8
}
