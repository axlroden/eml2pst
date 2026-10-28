use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, BufWriter, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use eml2pst::{FolderId, PstWriter, parse_eml};

/// Convert .eml files into an Outlook PST file.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// .eml files, .zip archives of .eml files, or directories. Directories
    /// are read recursively; subdirectories, and directories inside archives,
    /// become subfolders.
    inputs: Vec<PathBuf>,

    /// Output .pst file.
    #[arg(short, long)]
    output: PathBuf,

    /// Display name of the PST store [default: output file name].
    #[arg(long)]
    name: Option<String>,

    /// Folder for the inputs, as a "/"-separated path under the top of the store.
    #[arg(long, default_value = "Imported")]
    folder: String,

    /// Per-message folders. Either tab-separated "<folder path>\t<eml path>"
    /// lines, where a path may name a zip entry as "archive.zip!dir/message.eml",
    /// or (for a .jsonl file) one JSON object per line:
    /// {"folder": "Inbox", "path": "a.eml"} or
    /// {"folder": "Inbox", "zip": "b.zip", "entry": "dir/c.eml"}, with optional
    /// "unread": true, "draft": true and "categories": ["Label", ...].
    #[arg(long)]
    manifest: Option<PathBuf>,

    /// Mark messages unread instead of read.
    #[arg(long)]
    unread: bool,

    /// Keep plain-text messages plain text only. By default they also get an
    /// HTML rendering of the same text, because New Outlook for Mac shows
    /// imported plain-text bodies as empty or as their first letter.
    #[arg(long)]
    plain_text_only: bool,

    /// Attach forwarded messages as .eml files instead of Outlook items. New
    /// Outlook for Mac's PST import drops attached Outlook items but keeps
    /// .eml files.
    #[arg(long)]
    forwards_as_eml: bool,
}

/// One message to convert.
struct Job {
    folder: String,
    source: Source,
    /// Overrides `--unread` for this message.
    unread: Option<bool>,
    draft: bool,
    categories: Vec<String>,
}

impl Job {
    fn new(folder: String, source: Source) -> Self {
        Job {
            folder,
            source,
            unread: None,
            draft: false,
            categories: Vec::new(),
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestLine {
    folder: String,
    path: Option<PathBuf>,
    zip: Option<PathBuf>,
    entry: Option<String>,
    unread: Option<bool>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    categories: Vec<String>,
}

/// Where one message comes from.
enum Source {
    File(PathBuf),
    ZipEntry(PathBuf, String),
}

impl Source {
    fn parse(spec: &str) -> Self {
        match spec.split_once(".zip!") {
            Some((archive, entry)) => {
                Source::ZipEntry(PathBuf::from(format!("{archive}.zip")), entry.to_owned())
            }
            None => Source::File(PathBuf::from(spec)),
        }
    }

    fn describe(&self) -> String {
        match self {
            Source::File(p) => p.display().to_string(),
            Source::ZipEntry(z, e) => format!("{}!{e}", z.display()),
        }
    }
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("eml2pst: {e}");
            ExitCode::FAILURE
        }
    }
}

struct Folders {
    cache: HashMap<String, FolderId>,
}

impl Folders {
    /// Creates (or reuses) the folder at a "/"-separated path under the store top.
    fn get<W: io::Write + io::Seek>(&mut self, pst: &mut PstWriter<W>, path: &str) -> FolderId {
        let mut current = pst.root();
        let mut key = String::new();
        for part in path.split('/').map(str::trim).filter(|p| !p.is_empty()) {
            key.push('/');
            key.push_str(part);
            current = match self.cache.get(&key) {
                Some(&id) => id,
                None => {
                    let id = pst
                        .find_folder(current, part)
                        .unwrap_or_else(|| pst.add_folder(current, part));
                    self.cache.insert(key.clone(), id);
                    id
                }
            };
        }
        current
    }
}

/// Keeps the most recently used archive open, since jobs from one archive are
/// usually consecutive.
#[derive(Default)]
struct Archives {
    current: Option<(PathBuf, zip::ZipArchive<BufReader<File>>)>,
}

impl Archives {
    fn read(&mut self, archive: &Path, entry: &str) -> io::Result<Vec<u8>> {
        if self.current.as_ref().is_none_or(|(p, _)| p != archive) {
            let zip = zip::ZipArchive::new(BufReader::new(File::open(archive)?))
                .map_err(io::Error::other)?;
            self.current = Some((archive.to_owned(), zip));
        }
        let (_, zip) = self.current.as_mut().expect("archive just opened");
        let mut file = zip.by_name(entry).map_err(io::Error::other)?;
        let mut data = Vec::with_capacity(file.size() as usize);
        file.read_to_end(&mut data)?;
        Ok(data)
    }
}

fn run(args: &Args) -> io::Result<()> {
    let mut jobs: Vec<Job> = Vec::new();
    for input in &args.inputs {
        collect(input, &args.folder, &mut jobs)?;
    }
    if let Some(manifest) = &args.manifest {
        read_manifest(manifest, &mut jobs)?;
    }
    if jobs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no .eml inputs given",
        ));
    }

    let name = args.name.clone().unwrap_or_else(|| {
        args.output.file_stem().map_or_else(
            || "Outlook Data File".into(),
            |s| s.to_string_lossy().into_owned(),
        )
    });
    let out = BufWriter::with_capacity(1 << 20, File::create(&args.output)?);
    let mut pst = PstWriter::new(out, &name, *uuid::Uuid::new_v4().as_bytes());
    pst.set_plain_text_only(args.plain_text_only);
    pst.set_forwards_as_eml(args.forwards_as_eml);
    let mut folders = Folders {
        cache: HashMap::new(),
    };
    let mut archives = Archives::default();

    let (mut written, mut failed) = (0usize, 0usize);
    for Job {
        folder,
        source,
        unread,
        draft,
        categories,
    } in &jobs
    {
        let raw = match source {
            Source::File(path) => fs::read(path),
            Source::ZipEntry(archive, entry) => archives.read(archive, entry),
        };
        let raw = match raw {
            Ok(raw) => raw,
            Err(e) => {
                eprintln!("skipping {}: {e}", source.describe());
                failed += 1;
                continue;
            }
        };
        let Some(mut msg) = parse_eml(&raw) else {
            eprintln!("skipping {}: not an email message", source.describe());
            failed += 1;
            continue;
        };
        msg.read = !unread.unwrap_or(args.unread);
        msg.unsent = *draft;
        msg.categories.clone_from(categories);
        let id = folders.get(&mut pst, folder);
        pst.add_message(id, &msg)?;
        written += 1;
    }
    pst.finish()?;
    eprintln!(
        "wrote {written} messages to {} ({failed} skipped)",
        args.output.display()
    );
    Ok(())
}

fn read_manifest(manifest: &Path, jobs: &mut Vec<Job>) -> io::Result<()> {
    let jsonl = is_ext(manifest, "jsonl");
    for (n, line) in BufReader::new(File::open(manifest)?).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let bad = |what: String| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}:{}: {what}", manifest.display(), n + 1),
            )
        };
        if !jsonl {
            let (folder, path) = line
                .split_once('\t')
                .ok_or_else(|| bad("expected <folder>\\t<path>".into()))?;
            jobs.push(Job::new(folder.to_owned(), Source::parse(path)));
            continue;
        }
        let entry: ManifestLine = serde_json::from_str(&line).map_err(|e| bad(e.to_string()))?;
        let source = match (entry.path, entry.zip, entry.entry) {
            (Some(path), None, None) => Source::File(path),
            (None, Some(zip), Some(name)) => Source::ZipEntry(zip, name),
            _ => return Err(bad("give either \"path\" or \"zip\" and \"entry\"".into())),
        };
        jobs.push(Job {
            folder: entry.folder,
            source,
            unread: entry.unread,
            draft: entry.draft,
            categories: entry.categories,
        });
    }
    Ok(())
}

fn is_ext(path: &Path, ext: &str) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

/// Adds `path` (a file, an archive, or a directory walked recursively) to
/// `jobs`, mapping subdirectories to subfolders of `folder`.
fn collect(path: &Path, folder: &str, jobs: &mut Vec<Job>) -> io::Result<()> {
    if is_ext(path, "zip") && path.is_file() {
        let zip =
            zip::ZipArchive::new(BufReader::new(File::open(path)?)).map_err(io::Error::other)?;
        let mut names = Vec::new();
        for name in zip.file_names() {
            let name = name.map_err(io::Error::other)?;
            if is_ext(Path::new(name.as_ref()), "eml") {
                names.push(name.into_owned());
            }
        }
        names.sort_unstable();
        for name in names {
            let sub = match name.rsplit_once('/') {
                Some((dir, _)) => format!("{folder}/{dir}"),
                None => folder.to_owned(),
            };
            jobs.push(Job::new(sub, Source::ZipEntry(path.to_owned(), name)));
        }
        return Ok(());
    }
    if !path.is_dir() {
        jobs.push(Job::new(folder.to_owned(), Source::File(path.to_owned())));
        return Ok(());
    }
    let mut entries: Vec<_> = fs::read_dir(path)?.collect::<io::Result<_>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let p = entry.path();
        if p.is_dir() {
            let sub = format!("{folder}/{}", entry.file_name().to_string_lossy());
            collect(&p, &sub, jobs)?;
        } else if is_ext(&p, "eml") || is_ext(&p, "zip") {
            collect(&p, folder, jobs)?;
        }
    }
    Ok(())
}
