"""Independent Unicode PST dumper and validator.

Checks header and page CRCs, block signatures, B-tree ordering and AMap
consistency, and decodes every node (heaps, property contexts, tables).
Files using permutative encoding need --permute-table pointing at a copy of
the outlook-pst crate's src/encode/mod.rs.

Usage: python3 -I pstdump.py FILE [--raw] [--permute-table permute.rs-or-mod.rs]
"""
import struct, sys, zlib, re

def crc(data):
    return (~zlib.crc32(data, 0xFFFFFFFF)) & 0xFFFFFFFF

def sig(ib, bid):
    v = (ib ^ bid) & 0xFFFFFFFF
    return ((v >> 16) ^ v) & 0xFFFF

PERM_I = None
def load_perm(path):
    global PERM_I
    txt = open(path).read()
    m = re.search(r"KEY_DATA: \[u8; 768\] = \[(.*?)\];", txt, re.S)
    nums = [int(x) for x in re.findall(r"\d+", m.group(1))]
    PERM_I = bytes(nums[512:768])

class Pst:
    def __init__(self, path):
        self.f = open(path, 'rb').read()
        self.errors = []
        self.alloc = []  # (ib, size, what)
        h = self.f[:564]
        magic, crcp = struct.unpack_from('<II', h, 0)
        assert magic == 0x4E444221, hex(magic)
        if crcp != crc(h[8:8+471]): self.err('header partial crc')
        wver, = struct.unpack_from('<H', h, 10)
        assert wver >= 23, wver
        self.crypt = h[513]
        (self.crcfull,) = struct.unpack_from('<I', h, 524)
        if self.crcfull != crc(h[8:524]): self.err('header full crc')
        self.bidNextP, self.unique = struct.unpack_from('<QI', h, 32)
        self.rgnid = struct.unpack_from('<32I', h, 44)
        (self.eof, self.amapLast, self.amapFree, self.pmapFree,
         nbt_bid, nbt_ib, bbt_bid, bbt_ib) = struct.unpack_from('<QQQQQQQQ', h, 184)
        self.fAMapValid = h[248]
        self.bidNextB, = struct.unpack_from('<Q', h, 516)
        self.rgbFM = h[256:384]; self.rgbFP = h[384:512]
        self.nbt = {}; self.bbt = {}
        self.walk(nbt_ib, nbt_bid, 0x81, self.nbt)
        self.walk(bbt_ib, bbt_bid, 0x80, self.bbt)

    def err(self, m):
        self.errors.append(m)

    def page(self, ib, ptype, bid=None):
        p = self.f[ib:ib+512]
        t, t2, wsig, pcrc, pbid = struct.unpack_from('<BBHIQ', p, 496)
        if t != ptype or t2 != ptype: self.err(f'page {ib:#x} type {t:#x}/{t2:#x} want {ptype:#x}')
        if pcrc != crc(p[:496]): self.err(f'page {ib:#x} crc')
        if bid is not None and pbid != bid: self.err(f'page {ib:#x} bid {pbid:#x} want {bid:#x}')
        if ptype in (0x80, 0x81, 0x86) and wsig != sig(ib, pbid): self.err(f'page {ib:#x} sig')
        self.alloc.append((ib, 512, f'page{ptype:#x}'))
        return p

    def walk(self, ib, bid, ptype, out, lo=None):
        p = self.page(ib, ptype, bid)
        cEnt, cEntMax, cbEnt, cLevel = p[488:492]
        prev = None
        for i in range(cEnt):
            e = p[i*cbEnt:(i+1)*cbEnt]
            key = struct.unpack_from('<Q', e)[0]
            if prev is not None and key <= prev: self.err(f'btree {ib:#x} keys unsorted')
            if i == 0 and lo is not None and key != lo: self.err(f'btree {ib:#x} first key {key:#x} != parent key {lo:#x}')
            prev = key
            if cLevel > 0:
                cbid, cib = struct.unpack_from('<QQ', e, 8)
                self.walk(cib, cbid, ptype, out, key)
            elif ptype == 0x81:
                nid, bidData, bidSub, nidParent = struct.unpack_from('<QQQI', e)
                out[nid] = (bidData, bidSub, nidParent)
            else:
                bbid, bib, cb, cref = struct.unpack_from('<QQHH', e)
                out[bbid & ~1] = (bib, cb, cref)

    def block(self, bid):
        bid &= ~1
        ib, cb, cref = self.bbt[bid]
        size = (cb + 16 + 63) // 64 * 64
        b = self.f[ib:ib+size]
        tcb, wsig, bcrc, tbid = struct.unpack_from('<HHIQ', b, size-16)
        data = b[:cb]
        if tcb != cb: self.err(f'block {bid:#x} cb {tcb} != {cb}')
        if tbid != bid: self.err(f'block {bid:#x} trailer bid {tbid:#x}')
        if wsig != sig(ib, bid): self.err(f'block {bid:#x} sig')
        if bcrc != crc(data): self.err(f'block {bid:#x} crc')
        self.alloc.append((ib, size, f'block{bid:#x}'))
        if not (bid & 2) and self.crypt == 1:
            data = bytes(PERM_I[x] for x in data)
        return data

    def data(self, bid):
        if bid == 0: return b''
        b = self.block(bid)
        if bid & 2:
            btype, lvl, cent, total = struct.unpack_from('<BBHI', b)
            assert btype == 1, btype
            out = b''.join(self.data(x) for x in struct.unpack_from(f'<{cent}Q', b, 8))
            if len(out) != total: self.err(f'xblock {bid:#x} total {total} != {len(out)}')
            return out
        return b

    def blocks(self, bid):
        """List of data blocks (for HN multi-block)."""
        b = self.block(bid)
        if bid & 2:
            btype, lvl, cent, total = struct.unpack_from('<BBHI', b)
            r = []
            for x in struct.unpack_from(f'<{cent}Q', b, 8): r += self.blocks(x)
            return r
        return [b]

    def subnodes(self, bid):
        if bid == 0: return {}
        b = self.block(bid)
        btype, lvl, cent = struct.unpack_from('<BBH', b)
        assert btype == 2, btype
        out = {}
        for i in range(cent):
            if lvl == 0:
                nid, bd, bs = struct.unpack_from('<QQQ', b, 8 + i*24)
                out[nid & 0xFFFFFFFF] = (bd, bs)
            else:
                nid, bs = struct.unpack_from('<QQ', b, 8 + i*16)
                out.update(self.subnodes(bs))
        return out

# ---------------- LTP ----------------
class Heap:
    def __init__(self, blocks):
        self.blocks = blocks
        b0 = blocks[0]
        self.ibHnpm, self.bSig, self.clientSig, self.hidUserRoot, self.fill = struct.unpack_from('<HBBII', b0)
        assert self.bSig == 0xEC, hex(self.bSig)
        self.allocs = []
        for i, b in enumerate(blocks):
            ibHnpm = struct.unpack_from('<H', b)[0]
            cAlloc, cFree = struct.unpack_from('<HH', b, ibHnpm)
            offs = struct.unpack_from(f'<{cAlloc+1}H', b, ibHnpm + 4)
            self.allocs.append([b[offs[j]:offs[j+1]] for j in range(cAlloc)])

    def get(self, hid):
        if hid == 0: return b''
        assert hid & 0x1F == 0, f'hid type {hid:#x}'
        idx = (hid >> 5) & 0x7FF; blk = hid >> 16
        return self.allocs[blk][idx-1]

def bth(heap, hid):
    h = heap.get(hid)
    bType, cbKey, cbEnt, lvls, hidRoot = struct.unpack_from('<BBBBI', h)
    assert bType == 0xB5
    recs = []
    def rec(hr, lvl):
        if hr == 0: return
        d = heap.get(hr)
        if lvl == 0:
            for i in range(0, len(d), cbKey+cbEnt): recs.append((d[i:i+cbKey], d[i+cbKey:i+cbKey+cbEnt]))
        else:
            for i in range(0, len(d), cbKey+4): rec(struct.unpack_from('<I', d, i+cbKey)[0], lvl-1)
    rec(hidRoot, lvls)
    return (cbKey, cbEnt, lvls), recs

FIXED = {0x2:2, 0x3:4, 0x4:4, 0x5:8, 0x6:8, 0x7:8, 0xA:4, 0xB:1, 0x14:8, 0x40:8, 0x48:16}

def fmt(ptype, raw):
    if ptype == 0x1F: return repr(raw.decode('utf-16-le', 'replace'))
    if ptype == 0x1E: return repr(raw)
    if ptype == 0x102: return raw.hex()
    if ptype == 0x3: return f'{struct.unpack("<i", raw[:4])[0]} ({struct.unpack("<I", raw[:4])[0]:#x})'
    if ptype == 0xB: return str(bool(raw[0]))
    if ptype in (0x40, 0x14): return f'{struct.unpack("<Q", raw[:8])[0]:#x}'
    if ptype == 0x2: return str(struct.unpack('<h', raw[:2])[0])
    return raw.hex()

def nidname(n):
    return f'{n:#x}(t{n & 0x1F:#x},i{n >> 5:#x})'

def dump_pc(pst, heap, subs):
    (ck, ce, lv), recs = bth(heap, heap.hidUserRoot)
    print(f'   PC cbKey={ck} cbEnt={ce} lvls={lv} n={len(recs)}')
    for k, v in recs:
        pid, = struct.unpack('<H', k); ptype, hnid = struct.unpack('<HI', v)
        if ptype in FIXED and FIXED[ptype] <= 4:
            raw = struct.pack('<I', hnid)
            src = 'inline'
        elif hnid & 0x1F == 0:
            raw = heap.get(hnid); src = f'hid {hnid:#x}'
        else:
            raw = pst.data(subs[hnid][0]); src = f'subnode {hnid:#x}'
        print(f'    {pid:04X}:{ptype:04X} [{src}] = {fmt(ptype, raw)[:200]}')

def dump_tc(pst, heap, subs):
    h = heap.get(heap.hidUserRoot)
    bType, cCols = h[0], h[1]
    rgib = struct.unpack_from('<4H', h, 2)
    hidRowIndex, hnidRows, hidIndex = struct.unpack_from('<III', h, 10)
    print(f'   TC cols={cCols} rgib={rgib} rowIndex={hidRowIndex:#x} rows={hnidRows:#x} deprecatedIndex={hidIndex:#x}')
    cols = []
    for i in range(cCols):
        tag, ib, cb, ibit = struct.unpack_from('<IHBB', h, 22 + i*8)
        cols.append((tag, ib, cb, ibit))
        print(f'    col {tag>>16:04X}:{tag & 0xFFFF:04X} ib={ib} cb={cb} bit={ibit}')
    (ck, ce, lv), idx = bth(heap, hidRowIndex)
    print(f'    rowindex cbKey={ck} cbEnt={ce} lvls={lv} n={len(idx)}: ' + ', '.join(f'{struct.unpack("<I",k)[0]:#x}->{struct.unpack("<I",v)[0]}' for k, v in idx))
    if hnidRows == 0: return
    if hnidRows & 0x1F == 0: rowsdata = [heap.get(hnidRows)]; src = 'hid'
    else: rowsdata = pst.blocks(subs[hnidRows][0]); src = 'subnode'
    rs = rgib[3]
    for bi, rd in enumerate(rowsdata):
        print(f'    rows[{src}] block {bi} len={len(rd)} rowsize={rs}')
        for r in range(len(rd) // rs):
            row = rd[r*rs:(r+1)*rs]
            ceb = row[rgib[2]:rgib[3]]
            parts = []
            for tag, ib, cb, ibit in cols:
                if not (ceb[ibit // 8] >> (7 - ibit % 8)) & 1: continue
                raw = row[ib:ib+cb]; ptype = tag & 0xFFFF
                if ptype not in FIXED or FIXED[ptype] > cb:
                    hnid = struct.unpack('<I', raw)[0]
                    if hnid & 0x1F == 0: raw = heap.get(hnid)
                    else: raw = pst.data(subs[hnid][0])
                parts.append(f'{tag>>16:04X}={fmt(ptype, raw)[:60]}')
            print('     ', ' | '.join(parts))

def dump_subnodes(pst, subs, depth):
    """Decodes the heaps in a subnode tree, descending into nested subnode
    trees (attachments, and messages embedded in them)."""
    for sn, (bd, bs) in subs.items():
        if bd == 0:
            continue
        b = pst.blocks(bd)  # also accounts the blocks for the AMap check
        s2 = pst.subnodes(bs)
        # Subnodes of type NID_TYPE_LTP hold raw property values, not heaps.
        if sn & 0x1F != 0x1F and b and len(b[0]) >= 4 and b[0][2] == 0xEC:
            print(f'{"  " * depth}SUBNODE {nidname(sn)}:')
            h2 = Heap(b)
            if h2.clientSig == 0xBC: dump_pc(pst, h2, s2)
            elif h2.clientSig == 0x7C: dump_tc(pst, h2, s2)
        dump_subnodes(pst, s2, depth + 1)

def dump_node(pst, nid, bidData, bidSub, parent, raw):
    print(f'NODE {nidname(nid)} data={bidData:#x} sub={bidSub:#x} parent={parent:#x}')
    if bidData == 0: return
    blocks = pst.blocks(bidData)
    subs = pst.subnodes(bidSub)
    for sn, (bd, bs) in subs.items():
        print(f'   subnode {nidname(sn)} data={bd:#x} sub={bs:#x}')
    if raw:
        print('   raw', b''.join(blocks).hex())
    if len(blocks[0]) >= 4 and blocks[0][2] == 0xEC:
        heap = Heap(blocks)
        print(f'   HN clientSig={heap.clientSig:#x} root={heap.hidUserRoot:#x} fill={heap.fill:#x} allocs={[len(a) for a in heap.allocs]}')
        if heap.clientSig == 0xBC: dump_pc(pst, heap, subs)
        elif heap.clientSig == 0x7C: dump_tc(pst, heap, subs)
        dump_subnodes(pst, subs, 1)
    else:
        print('   raw', b''.join(blocks).hex()[:400])

def check_amap(pst):
    # Expected allocations: everything we touched + header region + map pages
    bits = {}
    ib = 0x4400; i = 0
    reserved = {}
    while ib < pst.eof:
        p = pst.page(ib, 0x84, ib)
        bits[ib] = p[:496]
        n = 1; nxt = ib + 512
        if i % 8 == 0: pst.page(nxt, 0x83, nxt); n += 1; nxt += 512
        if i >= 128 and (i - 128) % 496 == 0: pst.page(nxt, 0x82, nxt); n += 1; nxt += 512
        if i >= 8192 and (i - 8192) % (496*64) == 0: pst.page(nxt, 0x85, nxt); n += 1
        reserved[ib] = n * 512
        ib += 0x3E000; i += 1
    used = set()
    for a, size, what in pst.alloc:
        for s in range(a, a + size, 64): used.add(s)
    for base, bm in bits.items():
        for s in range(base, min(base + 496*8*64, pst.eof), 64):
            k = (s - base) // 64
            bit = (bm[k // 8] >> (7 - k % 8)) & 1
            mapped = s in used or (s - base) < reserved[base]
            if bit and not mapped: pst.err(f'amap: {s:#x} marked but unused')
            if not bit and (s in used or (s - base) < reserved[base]): pst.err(f'amap: {s:#x} used but free')

if __name__ == '__main__':
    args = sys.argv[1:]
    raw = '--raw' in args
    if '--permute-table' in args:
        load_perm(args[args.index('--permute-table') + 1])
    pst = Pst(args[0])
    print(f'crypt={pst.crypt} eof={pst.eof:#x} amapLast={pst.amapLast:#x} amapFree={pst.amapFree:#x} pmapFree={pst.pmapFree:#x} amapValid={pst.fAMapValid} bidNextB={pst.bidNextB:#x} bidNextP={pst.bidNextP:#x} unique={pst.unique:#x}')
    print('rgnid', [hex(x) for x in pst.rgnid])
    print('rgbFM[:4]', pst.rgbFM[:4].hex(), 'rgbFP[:4]', pst.rgbFP[:4].hex())
    for nid in sorted(pst.nbt):
        bd, bs, parent = pst.nbt[nid]
        dump_node(pst, nid, bd, bs, parent, raw)
    check_amap(pst)
    print('ERRORS:', pst.errors if pst.errors else 'none')
    sys.exit(1 if pst.errors else 0)
