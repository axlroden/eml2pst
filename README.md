# eml2pst

Converts `.eml` email files (RFC 5322 / MIME) into an Outlook PST file. It is a
single static binary for macOS, Linux and Windows, and it does not need Outlook
or MAPI.

The PST writer is implemented from the [MS-PST] open specification. It produces
Unicode (64-bit) PST files, and the store layout matches the blank PST that
Outlook creates.

## Status

Early. Files written by eml2pst are read cleanly by Microsoft's
[`outlook-pst`](https://crates.io/crates/outlook-pst) crate, by libpff
(`pffinfo`/`pffexport`) and by the bundled validator. They have **not yet been
opened in Outlook or checked with `scanpst.exe`**.

What is mapped from each message:

- subject as written (plus normalized subject and conversation topic)
- From as the represented sender and, when it differs, the Sender header as
  the actual sender (Outlook shows "Sender on behalf of From")
- To/Cc/Bcc recipients with SMTP one-off entry IDs
- sent date, and received date (from the first `Received:` header)
- plain text body (all text/plain body parts, verbatim; text derived from the
  HTML when there is none); HTML body as `PidTagHtml` (UTF-8) and, for every
  Outlook version, as compressed RTF (`PidTagRtfCompressed`, the HTML
  encapsulated per MS-OXRTFEX and LZFu-compressed per MS-OXRTFCP)
- importance (Importance, X-Priority, Priority, X-MSMail-Priority) and
  sensitivity (Sensitivity) headers
- S/MIME: clear-signed mail becomes `IPM.Note.SMIME.MultipartSigned` with the
  signed entity in `smime.p7m`, so Outlook can verify the signature; encrypted
  mail becomes `IPM.Note.SMIME`
- the raw header block as `PidTagTransportMessageHeaders`, plus Message-ID,
  In-Reply-To and References
- forwarded messages (`message/rfc822` parts) as attached Outlook items, with
  their own recipients and attachments
- attachments of any size, with file name, MIME type and Content-ID; inline
  (cid-referenced) parts are hidden attachments; nameless parts are named by
  type (`invite.ics`, `winmail.dat`, `attachment2.pdf`, ...)
- read/unread, draft (unsent) and categories, per message from a JSONL
  manifest
- meeting invitations, responses and cancellations (iCalendar `text/calendar`
  parts) become Outlook meeting items: `IPM.Schedule.Meeting.Request`,
  `.Resp.Pos`/`.Neg`/`.Tent` (by the reply's PARTSTAT; counter-proposals are
  tentative with the proposed times) and `.Canceled`, with start/end, location,
  busy status, reminders, global object ID, time zone and, for recurring series,
  the Outlook recurrence pattern (daily, weekly, monthly and yearly rules, with
  COUNT/UNTIL ends, EXDATE deletions, and exceptions: occurrences that an
  invitation moves, renames or relocates through `RECURRENCE-ID` overrides
  become Outlook exception objects, cancelled ones deletions). Times in a TZID
  are resolved through the VTIMEZONE the invitation carries; a series given in
  UTC recurs at a fixed UTC time, as RFC 5545 defines
- TNEF (`winmail.dat`) from Outlook is unpacked: its MAPI properties restore the
  original message class (for example meeting responses), RTF body and named
  properties, and its attachments become real attachments

## Usage

```bash
cargo build --release
```

```bash
./target/release/eml2pst -o mail.pst --folder "Imported" path/to/emls/ more.eml archive.zip
```

- Inputs can be `.eml` files, directories (read recursively) and `.zip` archives
  of `.eml` files. Subdirectories, and directories inside archives, become
  subfolders of `--folder` (a `/`-separated path; `--folder ""` puts them at the
  top of the store).
- `--manifest FILE` puts each message into its own folder. A `.jsonl` manifest
  has one JSON object per line:

  ```json
  {"folder": "Inbox", "path": "mail/a.eml"}
  {"folder": "Drafts", "zip": "batch-0001.zip", "entry": "b.eml", "draft": true}
  {"folder": "Inbox", "path": "c.eml", "unread": true, "categories": ["Work", "Travel"]}
  ```

  Any other manifest is read as tab-separated `<folder path>\t<eml path>` lines,
  where a path can name a zip entry as `archive.zip!dir/message.eml`. Use JSONL
  when entry names can contain tabs or newlines.
- `--name` sets the store display name (default: the output file name).
- `--unread` marks messages unread.
- `--plain-text-only` keeps plain-text messages without the HTML rendering
  added for New Outlook for Mac (see Known gaps).
- `--forwards-as-eml` attaches forwarded messages as `.eml` files instead of
  Outlook items, for New Outlook for Mac, whose import drops attached Outlook
  items. Recurrence exceptions stay Outlook items.

Messages that cannot be read or parsed are reported and skipped.

## Known gaps

- New Outlook for Mac (16.114, File > Import) imports eml2pst files: folders,
  senders, recipients, dates, conversations, HTML bodies, attachments and
  meeting items (it generates their calendar data) all come through. Its
  importer writes every plain-text body into MIME as raw UTF-16 labelled UTF-8,
  which shows as an empty or one-letter body, whatever the PST stores (Aspose
  users report the same). eml2pst therefore also writes an HTML rendering of
  plain text, which New Outlook shows instead; `--plain-text-only` turns that
  off. Its calendar data for imported meetings, built from eml2pst's
  properties, matches the source: recurrence, deletions, moved occurrences, time
  zone, reminder and attendees. The importer does not follow attached Outlook
  items (PtypObject attachments) at all, wherever in the file they sit: a
  forwarded message, and the hidden object of each recurrence exception, arrive
  as an 8-byte attachment holding the object's reference. With
  `--forwards-as-eml` forwards are attached as `.eml` files, which it keeps;
  the exception stubs remain, but the exceptions themselves come through in the
  meeting's calendar data.

- Recurrence rules Outlook has no pattern for (several BYMONTHDAY values,
  BYSETPOS lists, ...) are written exactly when bounded (COUNT/UNTIL): as a
  daily pattern over the occurrence dates with the other days deleted. Unbounded
  ones, and rules with several occurrences a day (BYHOUR, ...), cannot be
  expressed in Outlook at all; the series is then a single meeting and the
  `.ics` stays attached. Calendar parts with METHOD:PUBLISH or no method stay
  ordinary messages with the `.ics` attached.
- Some senders put HTML entities (`&nbsp;`) into their text/plain parts; the
  plain body keeps them as sent.
- Blocks are written unencoded (`NDB_CRYPT_NONE`). This is valid, but Outlook
  itself defaults to permutative ("compressible") encoding.
- libpff 20231205 prints `unable to retrieve local descriptor identifier: 1649`
  for messages without attachments. This is a libpff lookup bug: it reports a
  missing subnode as found. The message still exports correctly. `pffexport`
  also does not export the attachments of meeting items (such as exception
  objects), and Microsoft's `outlook-pst` reader looks for an attached item in
  the parent message's subnodes instead of the attachment's, so it cannot open
  attached Outlook items; libpff and pstsdk use the attachment's subnodes, as
  eml2pst writes them.

## Development

See [AGENTS.md](AGENTS.md).

## License

Copyright (c) 2026 Alex Rodenberg. Licensed under the GNU Affero General Public
License, version 3 only; see [LICENSE](LICENSE).

[MS-PST]: https://learn.microsoft.com/en-us/openspecs/office_file_formats/ms-pst/141923d5-15ab-4ef1-a524-6dce75aae546
