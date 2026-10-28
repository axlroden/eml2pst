# eml2pst

Convert `.eml` email files into an Outlook PST file. Runs on macOS, Linux and
Windows; Outlook is not needed.

## Install

Download the archive for your system from the
[releases page](https://github.com/axlroden/eml2pst/releases), unpack it and
put `eml2pst` (`eml2pst.exe` on Windows) somewhere on your `PATH`. There are
builds for Linux (x86_64, aarch64; static, any distribution), macOS (one binary
for Apple silicon and Intel) and Windows (x86_64, arm64).

On macOS, a binary downloaded with a browser is blocked until you clear its
quarantine flag:

```bash
xattr -d com.apple.quarantine eml2pst
```

Or build it yourself with `cargo build --release`; the binary is
`target/release/eml2pst`.

## Convert

Files, folders and zip archives of `.eml` files:

```bash
eml2pst -o mail.pst path/to/emls/ archive.zip one.eml
```

Subfolders (also inside zips) become Outlook folders under `Imported`. Use
`--folder "Mail/2024"` for a different place, or `--folder ""` for the top.

## Put each message in its own folder

Write a manifest, one JSON object per line:

```json
{"folder": "Inbox", "path": "mail/a.eml"}
{"folder": "Drafts", "zip": "batch-0001.zip", "entry": "b.eml", "draft": true}
{"folder": "Inbox", "path": "c.eml", "unread": true, "categories": ["Work"]}
```

```bash
eml2pst -o mail.pst --manifest mail.jsonl
```

`categories` become Outlook categories, which suits Gmail labels.

## Options

| Option | Effect |
| --- | --- |
| `-o, --output FILE` | PST file to write |
| `--folder PATH` | Folder for the inputs (default `Imported`) |
| `--manifest FILE` | Per-message folders and flags (`.jsonl`, or `folder<TAB>path` lines) |
| `--name NAME` | Name of the PST in Outlook (default: the file name) |
| `--unread` | Mark all messages unread |
| `--forwards-as-eml` | Attach forwarded emails as `.eml` files |
| `--plain-text-only` | Don't add an HTML copy to plain-text emails |

## Importing into New Outlook for Mac

Use File > Import and pick the PST. Add `--forwards-as-eml` when converting, or
forwarded emails arrive empty. Keep the default HTML copy of plain-text emails,
or their bodies show blank.

## More

- [docs/DETAILS.md](docs/DETAILS.md): what is converted, what it has been
  tested with, known limitations
- [AGENTS.md](AGENTS.md): development

## License

Copyright (c) 2026 Alex Rodenberg. Licensed under the GNU Affero General Public
License, version 3 only; see [LICENSE](LICENSE).
