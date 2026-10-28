# eml2pst details

What eml2pst maps from each email, what it has been verified with, and its
known limitations. For how to use it, see the [README](../README.md).

eml2pst writes Unicode (64-bit) PST files, implemented from Microsoft's
[MS-PST] open specification, with the store layout of the blank PST Outlook
creates.

## What it converts

**Messages**

- Subject (as written), sender, and the Sender header when mail was sent on
  someone's behalf (Outlook shows "Sender on behalf of From")
- To/Cc/Bcc recipients, sent date, and received date (from the first
  `Received:` header)
- Bodies: plain text (verbatim), HTML, and compressed RTF generated from the
  HTML so every Outlook version can show it. Plain-text messages also get an
  HTML rendering of the same text (see [Compatibility](#compatibility))
- The raw header block, Message-ID, In-Reply-To and References, so replies
  thread into conversations
- Attachments of any size, with file name, MIME type and Content-ID; images
  referenced from the HTML are hidden attachments; parts without a name are
  named by type (`invite.ics`, `attachment2.pdf`, ...)
- Forwarded messages (`message/rfc822`) as attached Outlook items with their
  own recipients and attachments, or as `.eml` files with `--forwards-as-eml`
- Importance (Importance, X-Priority, Priority, X-MSMail-Priority) and
  sensitivity (Sensitivity) headers
- S/MIME: signed mail keeps its signed content so Outlook can verify the
  signature; encrypted mail is kept for Outlook to decrypt
- Read/unread, drafts and categories (for example Gmail labels), set per
  message in a JSONL manifest

**Meetings**

- Invitations, responses (accepted, declined, tentative, counter-proposals)
  and cancellations sent as iCalendar become real Outlook meeting items, with
  start and end, location, busy status, reminder, attendees and time zone
- Recurring series get Outlook's recurrence pattern: daily, weekly, monthly and
  yearly rules, end dates or counts, deleted dates, and occurrences the
  invitation moves, renames or relocates. Bounded rules Outlook has no pattern
  for (for example the 1st and 15th of each month) are written as their exact
  dates
- Time zones are taken from the definitions inside each invitation

**Outlook's own data**

- `winmail.dat` (TNEF) from Outlook is unpacked: the original message class
  (for example a meeting acceptance), RTF body and properties are restored and
  its attachments become real attachments

## Compatibility

| Reader | Result |
| --- | --- |
| Microsoft's [`outlook-pst`](https://crates.io/crates/outlook-pst) crate | Reads everything except attached Outlook items (see below) |
| libpff (`pffinfo`, `pffexport`) | Reads everything, including RTF bodies and attached items |
| `tools/pstdump.py` (bundled validator) | No errors on a five-year, 31,000-message mailbox |
| New Outlook for Mac 16.114 (File > Import) | Imports, with the limits below |
| Outlook for Windows, `scanpst.exe` | Not tested yet |

New Outlook for Mac's import works through folders, senders, recipients, dates,
conversations, HTML bodies, attachments and meetings; the calendar data it
builds for meetings matches the source, recurrence exceptions included. Its
importer has two faults that appear to affect any PST, not only eml2pst's:

- It writes plain-text bodies into its own copy of each message as raw UTF-16
  labelled UTF-8, so they show as empty or as their first letter (Aspose users
  [report the same](https://forum.aspose.com/t/created-pst-files-not-working-with-new-outlook-on-mac/330391)).
  eml2pst therefore adds an HTML rendering of the same text, which it shows
  instead. `--plain-text-only` turns this off.
- It does not follow attached Outlook items. A forwarded message, and the
  hidden object of each moved meeting occurrence, arrive as an 8-byte
  attachment. `--forwards-as-eml` attaches forwards as `.eml` files, which it
  keeps; moved occurrences still show correctly in the meeting itself.

Notes on the other readers:

- libpff 20231205 prints `unable to retrieve local descriptor identifier: 1649`
  for each message without attachments. This is a libpff lookup bug; the
  message still exports correctly. `pffexport` does not export the
  attachments of meeting items.
- `outlook-pst` looks for an attached Outlook item in the parent message
  instead of the attachment, so it cannot open them; libpff and Microsoft's
  pstsdk read them from the attachment, where eml2pst writes them.

## Known limitations

- Recurrence rules that never end and have no Outlook pattern, and rules with
  several occurrences a day, cannot be expressed in Outlook. The meeting is
  then a single occurrence with the `.ics` kept as an attachment. Calendar
  files sent without a request, reply or cancel method stay ordinary messages
  with the `.ics` attached.
- Some senders put HTML entities (`&nbsp;`) into their plain-text parts; the
  plain-text body keeps them as sent.
- Data is written unencoded (`NDB_CRYPT_NONE`). This is valid; Outlook itself
  defaults to "compressible encryption".

[MS-PST]: https://learn.microsoft.com/en-us/openspecs/office_file_formats/ms-pst/141923d5-15ab-4ef1-a524-6dce75aae546
