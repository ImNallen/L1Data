//! Text crossing IPC is LF-only with no BOM, which is what CodeMirror holds (it splits on `\r\n?|\n`).

use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use crate::model::{FileBinding, FileFormat, FileReadError, FileStamp, FileWriteError, SaveTarget};

#[derive(Debug)]
pub(crate) struct FileOpened {
    pub(crate) binding: FileBinding,
    pub(crate) name: String,
    pub(crate) text: String,
}

#[derive(Debug)]
pub(crate) struct FileSaved {
    pub(crate) binding: FileBinding,
    pub(crate) name: String,
}

/// Every tab's whole text rides each workspace save, a sync command on the main thread.
pub(crate) const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// As Linux's MAXSYMLINKS.
const MAX_LINK_HOPS: usize = 40;

const BOM: &[u8] = b"\xEF\xBB\xBF";

pub(crate) fn open(path: &str) -> Result<FileOpened, FileReadError> {
    let path = canonical(path)?;
    let file = open_nonblocking(Path::new(&path))?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(FileReadError::NotAFile);
    }
    let too_large = |bytes| FileReadError::TooLarge { bytes, limit: MAX_FILE_BYTES };
    if meta.len() > MAX_FILE_BYTES {
        return Err(too_large(meta.len()));
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    (&file).take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(too_large(file.metadata()?.len().max(bytes.len() as u64)));
    }
    let (text, format) = decode(&bytes)?;
    Ok(FileOpened { name: name_of(&path), binding: FileBinding { path, stamp: stamp(&bytes), format }, text })
}

pub(crate) fn save(target: SaveTarget, text: &str) -> Result<FileSaved, FileWriteError> {
    let (path, format, expected, taken) = match target {
        SaveTarget::Bound { binding, overwrite } => (binding.path, binding.format, Some((binding.stamp, overwrite)), Vec::new()),
        SaveTarget::New { path, format, taken } => (path, format.unwrap_or_default(), None, taken),
    };
    let path = canonical(&path)?;
    let target = resolve_links(Path::new(&path))?;
    let existing = match fs::metadata(&target) {
        Ok(meta) => Some(meta),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    if let Some(other) = taken.into_iter().find(|other| same_file(&path, existing.as_ref(), other)) {
        return Err(FileWriteError::OpenInAnotherNotebook { path: other });
    }
    if let Some(meta) = &existing {
        if !meta.is_file() {
            return Err(FileWriteError::NotAFile);
        }
        // Before the outside-change check, so the user is not asked whether to overwrite a read-only file.
        if !writable(&target, meta) {
            return Err(FileWriteError::PermissionDenied);
        }
        if let Some((stamp, overwrite)) = &expected {
            let disk = stamp_file(&target)?;
            if disk != *stamp && Some(&disk) != overwrite.as_ref() {
                return Err(FileWriteError::Changed { disk });
            }
        }
    }
    let bytes = encode(text, format);
    write_atomic(&target, &bytes, existing.as_ref())?;
    Ok(FileSaved { name: name_of(&path), binding: FileBinding { path, stamp: stamp(&bytes), format } })
}

pub(crate) fn decode(bytes: &[u8]) -> Result<(String, FileFormat), FileReadError> {
    let (bom, body) = match bytes.strip_prefix(BOM) {
        Some(body) => (true, body),
        None => (false, bytes),
    };
    let text = std::str::from_utf8(body).map_err(|e| FileReadError::NotUtf8 { offset: (bytes.len() - body.len() + e.valid_up_to()) as u64 })?;
    let crlf = text.find(['\r', '\n']).is_some_and(|i| text[i..].starts_with("\r\n"));
    Ok((to_lf(text), FileFormat { bom, crlf }))
}

pub(crate) fn encode(text: &str, format: FileFormat) -> Vec<u8> {
    let lf = to_lf(text);
    let body = if format.crlf { lf.replace('\n', "\r\n") } else { lf };
    let bom: &[u8] = if format.bom { BOM } else { b"" };
    [bom, body.as_bytes()].concat()
}

fn to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn stamp(bytes: &[u8]) -> FileStamp {
    FileStamp(format!("{}:{:x}", bytes.len(), Sha256::digest(bytes)))
}

/// Streams, with no size cap: an outside tool may have made the file huge.
fn stamp_file(path: &Path) -> io::Result<FileStamp> {
    let mut hasher = Sha256::new();
    let len = io::copy(&mut open_nonblocking(path)?, &mut hasher)?;
    Ok(FileStamp(format!("{len}:{:x}", hasher.finalize())))
}

/// `O_NONBLOCK` so a FIFO cannot hang the open; a regular file ignores it.
fn open_nonblocking(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, libc::O_NONBLOCK);
    options.open(path)
}

/// The canonical parent joined with the file name: collapses `..` and directory symlinks, but keeps a final
/// symlink's own name, so the tab's title is the name the user picked.
fn canonical(path: &str) -> io::Result<String> {
    let path = Path::new(path);
    let name = path.file_name().ok_or(io::ErrorKind::IsADirectory)?;
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let joined = fs::canonicalize(parent)?.join(name).into_os_string().into_string().map_err(|p| {
        io::Error::new(io::ErrorKind::InvalidData, format!("{} is not valid UTF-8", p.to_string_lossy()))
    })?;
    Ok(plain_windows_path(joined))
}

const WINDOWS_DEVICES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3",
    "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Windows' `canonicalize` answers in verbatim form, `\\?\C:\…` or `\\?\UNC\server\share\…`, which dialogs and
/// people do not take. The path stays verbatim when its plain form would name another file: Win32 trims a name's
/// trailing `.` or space, and reads `CON` or `com1.sql` as a device. A Unix path never starts with `\\?\`.
fn plain_windows_path(path: String) -> String {
    let plain = match path.strip_prefix(r"\\?\") {
        Some(rest) if rest.starts_with(r"UNC\") => format!(r"\{}", &rest[3..]),
        Some(rest) if rest.get(1..3) == Some(r":\") => rest.to_owned(),
        _ => return path,
    };
    let device = |name: &str| {
        let stem = name.split_once('.').map_or(name, |(stem, _)| stem).trim_end_matches(' ');
        WINDOWS_DEVICES.iter().any(|d| d.eq_ignore_ascii_case(stem))
    };
    if plain.split('\\').any(|name| name.ends_with(['.', ' ']) || device(name)) { path } else { plain }
}

#[cfg(unix)]
fn same_file(target: &str, meta: Option<&Metadata>, other: &str) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (meta, fs::metadata(other)) {
        (Some(a), Ok(b)) => (a.dev(), a.ino()) == (b.dev(), b.ino()),
        _ => target == other,
    }
}

/// std has no stable file id here, and NTFS and FAT match names regardless of case.
#[cfg(not(unix))]
fn same_file(target: &str, _meta: Option<&Metadata>, other: &str) -> bool {
    target.to_lowercase() == other.to_lowercase()
}

fn name_of(path: &str) -> String {
    Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Not `fs::canonicalize`, which fails on a dangling link.
fn resolve_links(path: &Path) -> io::Result<PathBuf> {
    let mut path = path.to_path_buf();
    for _ in 0..MAX_LINK_HOPS {
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let link = fs::read_link(&path)?;
                path = match path.parent() {
                    Some(dir) => dir.join(link),
                    None => link,
                };
            }
            Ok(_) => return Ok(path),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(path),
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other(format!("too many levels of symbolic links at {}", path.display())))
}

/// A read-only file is refused even when its directory would allow the rename that replaces it, so the atomic
/// replace is never a way around `chmod 444`.
#[cfg(unix)]
fn writable(target: &Path, _meta: &Metadata) -> bool {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(target.as_os_str().as_bytes()).is_ok_and(|c| unsafe { libc::access(c.as_ptr(), libc::W_OK) } == 0)
}

#[cfg(not(unix))]
fn writable(_target: &Path, meta: &Metadata) -> bool {
    !meta.permissions().readonly()
}

fn write_atomic(target: &Path, bytes: &[u8], existing: Option<&Metadata>) -> io::Result<()> {
    let mut file = Replacement::beside(target.to_owned(), existing)?;
    file.write_all(bytes)?;
    file.commit().map(drop)
}

/// The bytes go to a temp file in the target's own directory, so the rename stays on one filesystem and the
/// target holds either its old bytes or every new byte. Dropping it uncommitted unlinks the temp file.
pub(crate) struct Replacement {
    tmp: BufWriter<NamedTempFile>,
    target: PathBuf,
    permissions: Option<fs::Permissions>,
    bytes: u64,
}

impl Replacement {
    /// Follows links, so the rename replaces the link's target rather than the link.
    pub(crate) fn begin(path: &str) -> io::Result<Self> {
        let target = resolve_links(Path::new(&canonical(path)?))?;
        let existing = match fs::metadata(&target) {
            Ok(meta) => Some(meta),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        if let Some(meta) = &existing {
            if !meta.is_file() {
                return Err(io::Error::new(io::ErrorKind::IsADirectory, "it is a folder or a device, not a file"));
            }
            if !writable(&target, meta) {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
        }
        Self::beside(target, existing.as_ref())
    }

    fn beside(target: PathBuf, existing: Option<&Metadata>) -> io::Result<Self> {
        let dir = target.parent().ok_or(io::ErrorKind::IsADirectory)?;
        let tmp = temp_beside(dir, &target, existing)?;
        Ok(Self { tmp: BufWriter::new(tmp), target, permissions: existing.map(Metadata::permissions), bytes: 0 })
    }

    pub(crate) fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.tmp.write_all(bytes)?;
        self.bytes += bytes.len() as u64;
        Ok(())
    }

    pub(crate) fn commit(self) -> io::Result<u64> {
        let tmp = self.tmp.into_inner().map_err(io::IntoInnerError::into_error)?;
        if let Some(permissions) = self.permissions {
            // Creation applied the umask, which may have cleared some of the target's bits.
            tmp.as_file().set_permissions(permissions)?;
        }
        tmp.as_file().sync_all()?;
        tmp.persist(&self.target).map_err(|e| e.error)?;
        #[cfg(unix)]
        if let Some(dir) = self.target.parent()
            && let Err(e) = File::open(dir).and_then(|d| d.sync_all())
        {
            eprintln!("fsync of {} after replacing a file failed: {e}", dir.display());
        }
        Ok(self.bytes)
    }
}

/// Created with the target's mode, so the new bytes are never readable by anyone the old ones were not. A new file
/// gets 0o666 less the umask, as an editor's would.
fn temp_beside(dir: &Path, target: &Path, existing: Option<&Metadata>) -> io::Result<NamedTempFile> {
    let name: String = target.file_name().unwrap_or_default().to_string_lossy().chars().take(64).collect();
    let prefix = format!(".{name}.");
    let mut builder = tempfile::Builder::new();
    builder.prefix(&prefix).suffix(".l1data-tmp");
    if let Some(meta) = existing {
        builder.permissions(meta.permissions());
    } else {
        #[cfg(unix)]
        builder.permissions(std::os::unix::fs::PermissionsExt::from_mode(0o666));
    }
    builder.tempfile_in(dir)
}

impl From<io::Error> for FileReadError {
    fn from(e: io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::NotFound | io::ErrorKind::NotADirectory => Self::NotFound,
            io::ErrorKind::PermissionDenied => Self::PermissionDenied,
            io::ErrorKind::IsADirectory => Self::NotAFile,
            _ => Self::Io { message: e.to_string() },
        }
    }
}

impl From<io::Error> for FileWriteError {
    fn from(e: io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::NotFound | io::ErrorKind::NotADirectory => Self::NotFound,
            io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem => Self::PermissionDenied,
            io::ErrorKind::IsADirectory => Self::NotAFile,
            _ => Self::Io { message: e.to_string() },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    use std::time::{Duration, SystemTime};

    const PLAIN: FileFormat = FileFormat { bom: false, crlf: false };

    fn s(path: &Path) -> String {
        path.to_str().unwrap().to_owned()
    }

    fn canonical_dir(dir: &tempfile::TempDir) -> PathBuf {
        PathBuf::from(plain_windows_path(s(&fs::canonicalize(dir.path()).unwrap())))
    }

    fn temps(dir: &Path) -> Vec<String> {
        let names = fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned());
        names.filter(|n| n.ends_with(".l1data-tmp")).collect()
    }

    fn bound(binding: FileBinding) -> SaveTarget {
        SaveTarget::Bound { binding, overwrite: None }
    }

    fn binding_for(path: &Path, bytes: &[u8]) -> FileBinding {
        fs::write(path, bytes).unwrap();
        open(&s(path)).unwrap().binding
    }

    #[cfg(unix)]
    fn within_two_seconds<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || tx.send(f()));
        rx.recv_timeout(Duration::from_secs(2)).expect("returned within 2 s")
    }

    #[cfg(unix)]
    fn is_root() -> bool {
        unsafe { libc::geteuid() == 0 }
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn decode_strips_a_bom_and_flags_it() {
        assert_eq!(decode(b"\xEF\xBB\xBFselect 1;").unwrap(), ("select 1;".to_owned(), FileFormat { bom: true, crlf: false }));
        assert_eq!(decode(b"select 1;").unwrap(), ("select 1;".to_owned(), PLAIN));
    }

    #[test]
    fn decode_takes_crlf_from_the_first_break_and_normalizes_every_break() {
        assert_eq!(decode(b"a;\r\nb;\nc;\rd;").unwrap(), ("a;\nb;\nc;\nd;".to_owned(), FileFormat { bom: false, crlf: true }));
        assert_eq!(decode(b"a;\nb;\r\n").unwrap(), ("a;\nb;\n".to_owned(), PLAIN));
        assert_eq!(decode(b"a;\rb;\r\n").unwrap(), ("a;\nb;\n".to_owned(), PLAIN), "a lone CR first is not CRLF");
    }

    #[test]
    fn decode_reports_the_first_non_utf8_byte_counting_the_bom() {
        let bad = decode(b"\xEF\xBB\xBFa\xFF");
        assert!(matches!(bad, Err(FileReadError::NotUtf8 { offset: 4 })), "{bad:?}");
        let utf16 = decode(b"\xFF\xFEs\x00");
        assert!(matches!(utf16, Err(FileReadError::NotUtf8 { offset: 0 })), "{utf16:?}");
        let truncated = decode("é".as_bytes().split_last().unwrap().1);
        assert!(matches!(truncated, Err(FileReadError::NotUtf8 { offset: 0 })), "{truncated:?}");
    }

    #[test]
    fn decode_of_empty_and_bom_only_files_is_empty_text() {
        assert_eq!(decode(b"").unwrap(), (String::new(), PLAIN));
        assert_eq!(decode(BOM).unwrap(), (String::new(), FileFormat { bom: true, crlf: false }));
    }

    #[test]
    fn encode_round_trips_decode_for_every_format() {
        let texts = ["", "select 1;", "a;\nb;\n", "\n", "a\n\n\nb", "é ☃ 😀\nselect '\u{FEFF}';\n"];
        for text in texts {
            for (bom, crlf) in [(false, false), (false, true), (true, false), (true, true)] {
                let format = FileFormat { bom, crlf };
                let decoded = FileFormat { bom, crlf: crlf && text.contains('\n') };
                assert_eq!(decode(&encode(text, format)).unwrap(), (text.to_owned(), decoded), "{text:?} as {format:?}");
            }
        }
        assert_eq!(encode("a\nb\n", FileFormat { bom: true, crlf: true }), b"\xEF\xBB\xBFa\r\nb\r\n");
    }

    #[test]
    fn encode_normalizes_stray_breaks_and_never_writes_cr_cr_lf() {
        assert_eq!(encode("a\r\nb\rc\n", FileFormat { bom: false, crlf: true }), b"a\r\nb\r\nc\r\n");
        assert_eq!(encode("a\r\nb\rc\n", PLAIN), b"a\nb\nc\n");
    }

    #[test]
    fn stamp_is_the_length_and_the_sha256_hex() {
        assert_eq!(stamp(b"abc"), FileStamp("3:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into()));
        assert_eq!(stamp(b"abc"), stamp(b"abc"));
        assert_ne!(stamp(b"abc"), stamp(b"abd"));
    }

    #[test]
    fn open_reads_text_name_and_binding() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.sql");
        let bytes = b"\xEF\xBB\xBFselect 1;\r\nselect 2;\r\n";
        fs::write(&path, bytes).unwrap();
        let opened = open(&s(&path)).unwrap();
        assert_eq!((opened.name.as_str(), opened.text.as_str()), ("report.sql", "select 1;\nselect 2;\n"));
        assert_eq!(opened.binding.path, s(&canonical_dir(&dir).join("report.sql")));
        assert_eq!(opened.binding.stamp, stamp(bytes));
        assert_eq!(opened.binding.format, FileFormat { bom: true, crlf: true });
    }

    #[test]
    #[cfg(unix)]
    fn open_canonicalizes_the_folder_but_keeps_a_symlinks_own_name() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("a.sql"), "select 1;").unwrap();
        symlink("a.sql", dir.path().join("link.sql")).unwrap();
        let through_dots = open(&s(&dir.path().join("sub/../a.sql"))).unwrap();
        assert_eq!(through_dots.binding.path, s(&canonical_dir(&dir).join("a.sql")));
        let link = open(&s(&dir.path().join("link.sql"))).unwrap();
        assert_eq!((link.binding.path, link.name.as_str()), (s(&canonical_dir(&dir).join("link.sql")), "link.sql"));
        assert_eq!(link.text, "select 1;");
    }

    #[test]
    fn open_refuses_a_file_over_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dump.sql");
        File::create(&path).unwrap().set_len(MAX_FILE_BYTES + 1).unwrap();
        let result = open(&s(&path));
        assert!(matches!(result, Err(FileReadError::TooLarge { bytes, limit: MAX_FILE_BYTES }) if bytes == MAX_FILE_BYTES + 1), "{result:?}");
        File::create(&path).unwrap().set_len(MAX_FILE_BYTES).unwrap();
        assert_eq!(open(&s(&path)).unwrap().text.len() as u64, MAX_FILE_BYTES, "a file at the cap opens");
    }

    #[test]
    fn open_of_a_missing_path_or_a_directory_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let missing = open(&s(&dir.path().join("missing.sql")));
        assert!(matches!(missing, Err(FileReadError::NotFound)), "{missing:?}");
        let gone_folder = open(&s(&dir.path().join("gone/missing.sql")));
        assert!(matches!(gone_folder, Err(FileReadError::NotFound)), "{gone_folder:?}");
        let directory = open(&s(dir.path()));
        assert!(matches!(directory, Err(FileReadError::NotAFile)), "{directory:?}");
    }

    #[test]
    #[cfg(unix)]
    fn open_of_a_fifo_is_not_a_file_and_does_not_block() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pipe.sql");
        let c = std::ffi::CString::new(s(&path)).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o644) }, 0, "mkfifo");
        let result = within_two_seconds(move || open(&s(&path)));
        assert!(matches!(result, Err(FileReadError::NotAFile)), "{result:?}");
    }

    #[test]
    #[cfg(unix)]
    fn open_without_read_permission_is_permission_denied() {
        if is_root() {
            return eprintln!("skipped: root reads any file");
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.sql");
        fs::write(&path, "select 1;").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        let result = open(&s(&path));
        assert!(matches!(result, Err(FileReadError::PermissionDenied)), "{result:?}");
    }

    #[test]
    fn bound_save_writes_the_text_and_returns_the_stamp_of_the_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.sql");
        let binding = binding_for(&path, b"select 1;\n");
        let saved = save(bound(binding.clone()), "select 2;\n").unwrap();
        let disk = fs::read(&path).unwrap();
        assert_eq!(disk, b"select 2;\n");
        assert_eq!(saved.binding, FileBinding { stamp: stamp(&disk), ..binding });
        assert_eq!(saved.name, "a.sql");
        save(bound(saved.binding), "select 3;\n").expect("the returned binding saves again without a Changed");
        assert_eq!(fs::read(&path).unwrap(), b"select 3;\n");
        assert_eq!(temps(dir.path()), Vec::<String>::new());
    }

    #[test]
    fn bound_save_refuses_an_outside_change_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.sql");
        let binding = binding_for(&path, b"select 1;\n");
        fs::write(&path, "outside\n").unwrap();
        let result = save(bound(binding), "select 2;\n");
        assert!(matches!(&result, Err(FileWriteError::Changed { disk }) if *disk == stamp(b"outside\n")), "{result:?}");
        assert_eq!(fs::read(&path).unwrap(), b"outside\n");
        assert_eq!(temps(dir.path()), Vec::<String>::new());
    }

    #[test]
    fn bound_save_overwrites_only_the_version_the_user_was_shown() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.sql");
        let binding = binding_for(&path, b"v1\n");
        fs::write(&path, "v2\n").unwrap();
        let Err(FileWriteError::Changed { disk: v2 }) = save(bound(binding.clone()), "mine\n") else { panic!("v2 is an outside change") };
        fs::write(&path, "v3\n").unwrap();
        let stale = save(SaveTarget::Bound { binding: binding.clone(), overwrite: Some(v2) }, "mine\n");
        let Err(FileWriteError::Changed { disk: v3 }) = stale else { panic!("a consent to v2 does not cover v3: {stale:?}") };
        assert_eq!(v3, stamp(b"v3\n"));
        assert_eq!(fs::read(&path).unwrap(), b"v3\n");
        let saved = save(SaveTarget::Bound { binding, overwrite: Some(v3) }, "mine\n").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"mine\n");
        assert_eq!(saved.binding.stamp, stamp(b"mine\n"));
    }

    #[test]
    fn bound_save_ignores_a_touch_that_keeps_the_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.sql");
        let binding = binding_for(&path, b"select 1;\n");
        let later = SystemTime::now() + Duration::from_secs(3600);
        File::options().write(true).open(&path).unwrap().set_modified(later).unwrap();
        save(bound(binding), "select 2;\n").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"select 2;\n");
    }

    #[test]
    fn bound_save_treats_a_stamp_it_does_not_write_as_a_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.sql");
        let binding = FileBinding { stamp: FileStamp("v9:whatever".into()), ..binding_for(&path, b"select 1;\n") };
        let result = save(bound(binding), "select 2;\n");
        assert!(matches!(result, Err(FileWriteError::Changed { .. })), "{result:?}");
        assert_eq!(fs::read(&path).unwrap(), b"select 1;\n");
    }

    #[test]
    fn bound_save_recreates_a_deleted_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.sql");
        let binding = binding_for(&path, b"select 1;\n");
        fs::remove_file(&path).unwrap();
        save(bound(binding), "select 2;\n").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"select 2;\n");
    }

    #[test]
    fn bound_save_into_a_vanished_folder_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        let binding = binding_for(&dir.path().join("sub/a.sql"), b"select 1;\n");
        fs::remove_dir_all(dir.path().join("sub")).unwrap();
        let result = save(bound(binding), "select 2;\n");
        assert!(matches!(result, Err(FileWriteError::NotFound)), "{result:?}");
    }

    #[test]
    fn bound_save_keeps_the_bom_and_crlf_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.sql");
        let binding = binding_for(&path, b"\xEF\xBB\xBFa;\r\nb;\r\n");
        save(bound(binding), "a;\nb;\nc;\n").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"\xEF\xBB\xBFa;\r\nb;\r\nc;\r\n");
    }

    #[test]
    #[cfg(unix)]
    fn new_save_creates_a_file_with_the_umask_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new.sql");
        let previous = unsafe { libc::umask(0o022) };
        let saved = save(SaveTarget::New { path: s(&path), format: None, taken: vec![] }, "select 1;\n");
        unsafe { libc::umask(previous) };
        assert_eq!(saved.unwrap().binding.path, s(&canonical_dir(&dir).join("new.sql")));
        assert_eq!(mode(&path), 0o644);
    }

    #[test]
    fn new_save_replaces_an_existing_file_without_a_stamp_check() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.sql");
        fs::write(&path, "someone else's\n").unwrap();
        let saved = save(SaveTarget::New { path: s(&path), format: None, taken: vec![] }, "select 1;\n").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"select 1;\n");
        assert_eq!((saved.name.as_str(), saved.binding.stamp), ("a.sql", stamp(b"select 1;\n")));
    }

    #[test]
    fn new_save_writes_the_format_it_is_given_and_lf_without_bom_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.sql");
        let plain = save(SaveTarget::New { path: s(&path), format: None, taken: vec![] }, "a;\r\nb;\n").unwrap();
        assert_eq!((fs::read(&path).unwrap(), plain.binding.format), (b"a;\nb;\n".to_vec(), PLAIN));
        let format = FileFormat { bom: true, crlf: true };
        let kept = save(SaveTarget::New { path: s(&path), format: Some(format), taken: vec![] }, "a;\nb;\n").unwrap();
        assert_eq!((fs::read(&path).unwrap(), kept.binding.format), (b"\xEF\xBB\xBFa;\r\nb;\r\n".to_vec(), format));
    }

    #[test]
    #[cfg(unix)]
    fn save_keeps_the_targets_permission_bits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.sql");
        let binding = binding_for(&path, b"select 1;\n");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        save(bound(binding), "select 2;\n").unwrap();
        assert_eq!(mode(&path), 0o640);
    }

    #[test]
    #[cfg(unix)]
    fn the_temp_file_starts_with_no_permission_the_target_lacks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.sql");
        fs::write(&path, "select 1;\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let tmp = temp_beside(dir.path(), &path, Some(&fs::metadata(&path).unwrap())).unwrap();
        let meta = fs::metadata(tmp.path()).unwrap();
        assert_eq!((meta.len(), meta.permissions().mode() & 0o777 & !0o600), (0, 0), "created empty, with no bit beyond 0o600");
    }

    fn save_as(path: &Path, taken: &[&str]) -> Result<FileSaved, FileWriteError> {
        save(SaveTarget::New { path: s(path), format: None, taken: taken.iter().map(|&t| t.to_owned()).collect() }, "mine\n")
    }

    #[test]
    #[cfg(unix)]
    fn save_as_refuses_another_name_for_a_file_another_notebook_holds() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("real")).unwrap();
        symlink("real", dir.path().join("alias")).unwrap();
        symlink("real/a.sql", dir.path().join("link.sql")).unwrap();
        let held = binding_for(&dir.path().join("real/a.sql"), b"select 1;\n").path;
        fs::hard_link(dir.path().join("real/a.sql"), dir.path().join("hard.sql")).unwrap();
        for name in ["real/a.sql", "alias/a.sql", "real/../alias/a.sql", "link.sql", "hard.sql"] {
            let result = save_as(&dir.path().join(name), &["/elsewhere/b.sql", &held]);
            assert!(matches!(&result, Err(FileWriteError::OpenInAnotherNotebook { path }) if *path == held), "{name}: {result:?}");
        }
        assert_eq!(fs::read(dir.path().join("real/a.sql")).unwrap(), b"select 1;\n");
        assert_eq!(temps(&dir.path().join("real")), Vec::<String>::new());
        save_as(&dir.path().join("alias/b.sql"), &[&held]).expect("another file in the same folder saves");
    }

    #[test]
    #[cfg(unix)]
    fn save_as_refuses_the_path_of_another_notebooks_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("real")).unwrap();
        symlink("real", dir.path().join("alias")).unwrap();
        let held = binding_for(&dir.path().join("real/a.sql"), b"select 1;\n").path;
        fs::remove_file(dir.path().join("real/a.sql")).unwrap();
        let result = save_as(&dir.path().join("alias/a.sql"), &[&held]);
        assert!(matches!(&result, Err(FileWriteError::OpenInAnotherNotebook { path }) if *path == held), "{result:?}");
        assert!(!dir.path().join("real/a.sql").exists(), "nothing was written");
    }

    #[test]
    fn a_verbatim_windows_path_turns_plain_unless_plain_names_another_file() {
        let plain = |path: &str| plain_windows_path(path.to_owned());
        assert_eq!(plain(r"\\?\C:\Users\a\q.sql"), r"C:\Users\a\q.sql");
        assert_eq!(plain(r"\\?\UNC\server\share\q.sql"), r"\\server\share\q.sql");
        for kept in [r"\\?\C:\a\con.sql", r"\\?\C:\a\Com1", r"\\?\C:\a\q.sql.", r"\\?\C:\a \q.sql", r"\\?\Volume{4c1b}\q.sql"] {
            assert_eq!(plain(kept), kept);
        }
        for untouched in ["/Users/a/q.sql", r"C:\a\q.sql", r"\\server\share\q.sql"] {
            assert_eq!(plain(untouched), untouched);
        }
        assert_eq!(plain(r"\\?\C:\a\console.sql"), r"C:\a\console.sql");
    }

    #[test]
    #[cfg(windows)]
    fn a_saved_path_on_windows_is_plain() {
        let dir = tempfile::tempdir().unwrap();
        let saved = save_as(&dir.path().join("a.sql"), &[]).unwrap();
        assert!(!saved.binding.path.starts_with(r"\\?\"), "{}", saved.binding.path);
    }

    #[test]
    #[cfg(unix)]
    fn save_refuses_a_read_only_file_even_in_a_writable_folder() {
        if is_root() {
            return eprintln!("skipped: root writes any file");
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.sql");
        let binding = binding_for(&path, b"select 1;\n");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
        let inode = fs::metadata(&path).unwrap().ino();
        let result = save(bound(binding), "select 2;\n");
        assert!(matches!(result, Err(FileWriteError::PermissionDenied)), "{result:?}");
        let new = save(SaveTarget::New { path: s(&path), format: None, taken: vec![] }, "select 2;\n");
        assert!(matches!(new, Err(FileWriteError::PermissionDenied)), "{new:?}");
        assert_eq!((fs::read(&path).unwrap(), fs::metadata(&path).unwrap().ino()), (b"select 1;\n".to_vec(), inode));
        assert_eq!(temps(dir.path()), Vec::<String>::new());
    }

    #[test]
    #[cfg(unix)]
    fn save_in_a_folder_it_cannot_write_is_permission_denied_and_leaves_the_file() {
        if is_root() {
            return eprintln!("skipped: root writes any folder");
        }
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        fs::create_dir(&sub).unwrap();
        let binding = binding_for(&sub.join("a.sql"), b"select 1;\n");
        fs::set_permissions(&sub, fs::Permissions::from_mode(0o555)).unwrap();
        let result = save(bound(binding), "select 2;\n");
        let (bytes, left) = (fs::read(sub.join("a.sql")).unwrap(), temps(&sub));
        fs::set_permissions(&sub, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(result, Err(FileWriteError::PermissionDenied)), "{result:?}");
        assert_eq!((bytes, left), (b"select 1;\n".to_vec(), Vec::<String>::new()));
    }

    /// `save` refuses a target that is not a regular file before it makes a temp file, so the failed rename is
    /// reachable only here: a file cannot be renamed over a non-empty directory.
    #[test]
    fn a_failed_rename_removes_the_temp_file_and_leaves_the_old_contents() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a.sql");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("inner.sql"), "old\n").unwrap();
        let result = write_atomic(&target, b"new\n", None);
        assert!(result.is_err(), "renaming a file over a non-empty directory fails");
        assert_eq!(fs::read(target.join("inner.sql")).unwrap(), b"old\n");
        assert_eq!(temps(dir.path()), Vec::<String>::new());
    }

    #[test]
    #[cfg(unix)]
    fn save_through_a_symlink_writes_its_target_and_keeps_the_link() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("real.sql"), "select 1;\n").unwrap();
        let link = dir.path().join("link.sql");
        symlink("real.sql", &link).unwrap();
        let binding = open(&s(&link)).unwrap().binding;
        let saved = save(bound(binding), "select 2;\n").unwrap();
        assert_eq!(fs::read(dir.path().join("real.sql")).unwrap(), b"select 2;\n");
        assert_eq!(fs::read_link(&link).unwrap(), Path::new("real.sql"));
        assert_eq!((saved.binding.path, saved.name.as_str()), (s(&canonical_dir(&dir).join("link.sql")), "link.sql"));
        assert_eq!(temps(dir.path()), Vec::<String>::new());
    }

    #[test]
    #[cfg(unix)]
    fn save_through_a_relative_symlink_chain() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("c.sql"), "select 1;\n").unwrap();
        symlink("sub/b.sql", dir.path().join("a.sql")).unwrap();
        symlink("../c.sql", dir.path().join("sub/b.sql")).unwrap();
        let binding = open(&s(&dir.path().join("a.sql"))).unwrap().binding;
        save(bound(binding), "select 2;\n").unwrap();
        assert_eq!(fs::read(dir.path().join("c.sql")).unwrap(), b"select 2;\n");
        assert_eq!(fs::read_link(dir.path().join("a.sql")).unwrap(), Path::new("sub/b.sql"));
        assert_eq!(fs::read_link(dir.path().join("sub/b.sql")).unwrap(), Path::new("../c.sql"));
    }

    #[test]
    #[cfg(unix)]
    fn save_through_a_dangling_symlink_creates_its_target() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("link.sql");
        symlink("missing.sql", &link).unwrap();
        save(SaveTarget::New { path: s(&link), format: None, taken: vec![] }, "select 1;\n").unwrap();
        assert_eq!(fs::read(dir.path().join("missing.sql")).unwrap(), b"select 1;\n");
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "the link stays a link");
    }

    #[test]
    #[cfg(unix)]
    fn a_symlink_loop_is_an_io_error_not_a_hang() {
        let dir = tempfile::tempdir().unwrap();
        symlink("b.sql", dir.path().join("a.sql")).unwrap();
        symlink("a.sql", dir.path().join("b.sql")).unwrap();
        let path = s(&dir.path().join("a.sql"));
        let result = within_two_seconds(move || save(SaveTarget::New { path, format: None, taken: vec![] }, "select 1;\n"));
        assert!(matches!(result, Err(FileWriteError::Io { .. })), "{result:?}");
        assert_eq!(temps(dir.path()), Vec::<String>::new());
    }
}
