# eml2pst - agent notes

Rust crate and CLI that writes Unicode PST files from `.eml` messages. The format
reference is [MS-PST]; the code is laid out by its layers.

## Layout

- `src/ndb.rs` - Node Database: data blocks, XBLOCK/XXBLOCK data trees,
  SLBLOCK/SIBLOCK subnode trees, node and block B-tree pages, AMap/PMap/FMap/
  FPMap pages, density list and header. Write-once: blocks are appended as they
  are produced, and everything that indexes them is built in `NdbWriter::finish`.
- `src/ltp.rs` - heap-on-node, BTree-on-heap, property contexts (`build_pc`) and
  table contexts (`build_tc`).
- `src/messaging.rs` - store, the fixed folders and table templates, user
  folders, messages, recipients and attachments. The fixed objects mirror the
  blank PST that Outlook creates (`Empty.pst` in the `outlook-pst` crate's
  examples); keep them in sync with it when changing anything there.
- `src/named_props.rs` - named property map, seeded with the same 35 entries
  Outlook registers in a new store.
- `src/eml.rs` - MIME to `Message` mapping (mail-parser); then TNEF and meeting
  conversion.
- `src/tnef.rs` - TNEF (`winmail.dat`) decoder (MS-OXTNEF) and merge into the
  message.
- `src/ical.rs` - iCalendar reader: content lines, values, RRULE, VTIMEZONE
  evaluation (wall time <-> UTC), civil date arithmetic.
- `src/recurrence.rs` - RRULE to Outlook AppointmentRecurrencePattern, and the
  PidLidTimeZoneStruct / TZDEFINITION blobs (MS-OXOCAL 2.2.1.39-2.2.1.44).
- `src/meeting.rs` - iTIP METHOD/PARTSTAT to meeting message classes and
  appointment/meeting named properties, global object IDs, recurrence
  exceptions (hidden exception attachments).
- `src/rtf.rs` - LZFu compression/decompression (MS-OXRTFCP; the compressor
  reproduces the specification's example byte for byte) and HTML-in-RTF
  encapsulation (MS-OXRTFEX).
- `examples/inspect_eml.rs` (`FULL=1` for whole values) and
  `examples/read_pst.rs` (`READ_PST_JSON=1` for full bodies) - debugging aids;
  the latter reads through Microsoft's `outlook-pst`.
- `src/main.rs` - CLI: files, directories, zip archives, manifest.
- `tools/pstdump.py` - independent Python validator and dumper (CRCs,
  signatures, B-tree order, AMap consistency, decoded heaps/PCs/TCs). Exits
  non-zero on any finding.

## Toolchain

There is no system Rust here; use Nix: `nix develop` (see `flake.nix`), or
`nix shell nixpkgs#cargo nixpkgs#rustc nixpkgs#rustfmt nixpkgs#clippy nixpkgs#libpff`.

## Checks before handing off

```bash
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```

`tests/roundtrip.rs` writes PSTs and reads them back with Microsoft's
`outlook-pst` crate. For changes to the on-disk format, also run a generated
file through libpff and the validator:

```bash
cargo run -q -- -o /tmp/sample.pst --folder "" tests/fixtures/sample
```

```bash
python3 -I tools/pstdump.py /tmp/sample.pst | tail -1
```

```bash
pffexport -q -m all -t /tmp/sample /tmp/sample.pst
```

## Releasing

`.github/workflows/release.yml` builds the binaries (static musl on Linux,
universal on macOS, static CRT on Windows) and runs them on the sample
fixtures. To release, set the version in `Cargo.toml`, commit, and push a tag
`v<version>`; the workflow refuses a tag that does not match `Cargo.toml` and
publishes the archives and `SHA256SUMS` as a GitHub release. Run it by hand
(workflow_dispatch) to build without publishing.

## Format facts worth knowing

- Outlook grows files one AMap region (253,952 bytes) at a time; `ibFileEof` is
  always the end of the last region. Reserved pages sit at the start of each
  region: AMap, then PMap every 8 regions, FMap from region 128 every 496, FPMap
  from region 8192 every 31,744.
- Block CRCs are CRC-32 seeded with 0 and not inverted (`crc.rs`).
- Outlook writes a block reference count of 2 for a block that is used once.
- Empty strings and binaries in property contexts use HNID 0, as Outlook does.
  In table rows the cell is left unset instead, because `outlook-pst` rejects
  HNID 0 there.
- libpff refuses a store whose named property map has no entries, and reports
  an empty `PidTagHtml` as missing data; empty bodies are not written.
- Attached Outlook items (forwards, recurrence exceptions) are a PtypObject
  (subnode NID + size) in the attachment's PC; the message lives in the
  attachment's subnode tree, where libpff and pstsdk look. `outlook-pst` looks
  in the parent message's subnodes and cannot open them, and `pffexport` skips
  meeting attachments; check those with `tools/pstdump.py` (it walks nested
  subnode trees) or by exporting a copy written as IPM.Note.
- Named property IDs are assigned per store in registration order; tests resolve
  them from the PST's own name-to-ID map (see `named_id` in tests/roundtrip.rs).
- RTF bodies can be checked end to end: `pffexport -f all` writes both
  Message.rtf (libpff decompresses LZFu; it appends a NUL) and Message.html;
  de-encapsulating the RTF must give the HTML exactly.
- Bytes to compare against Outlook: the TZDEFINITION and global object ID unit
  tests in `recurrence.rs` and `meeting.rs` hold values taken from an
  Outlook-generated meeting response.
- Verified by importing into New Outlook for Mac (File > Import; its file
  picker needs a person to click). The importer keeps each item as an `.eml`
  under `~/Library/Group Containers/UBF8T346G9.Office/Outlook/Outlook 15
  Profiles/Main Profile/Omc/`, which shows exactly what it read. Not yet
  verified in Outlook for Windows or `scanpst.exe`; treat that as the open risk
  on every format change.

[MS-PST]: https://learn.microsoft.com/en-us/openspecs/office_file_formats/ms-pst/141923d5-15ab-4ef1-a524-6dce75aae546
