//! Opening a session log by the plain `.jsonl` path it is indexed under. Codex can
//! replace a cold rollout with a byte-identical `.jsonl.zst` sibling and restore
//! the plain file when the thread resumes; offsets always address decoded bytes.
use std::{
    ffi::OsString,
    fs::{File, Metadata},
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom},
    os::unix::fs::FileExt,
    path::{Path, PathBuf},
};

pub struct Source {
    pub file: File,
    pub meta: Metadata,
    pub compressed: bool,
}

/// The plain path a discovered file is indexed under, or `None` for a file that
/// is not a session log. Codex's temporary files end in `.tmp` and never match.
pub fn plain_path(path: &Path) -> Option<PathBuf> {
    match path.extension()?.to_str()? {
        "jsonl" => Some(path.to_path_buf()),
        "zst" if path.file_stem().map(Path::new)?.extension()? == "jsonl" => {
            Some(path.with_extension(""))
        }
        _ => None,
    }
}

fn compressed_path(plain: &Path) -> PathBuf {
    let mut path = OsString::from(plain);
    path.push(".zst");
    path.into()
}

/// The plain file, else its compressed sibling, or `None` when neither exists, as
/// when a harness deletes or archives the log after discovery listed it.
pub fn open(plain: &Path) -> io::Result<Option<Source>> {
    open_with(plain, |path| File::open(path))
}

/// [`open`] with the call that opens each candidate supplied, so a test can
/// change the files between attempts as a harness can.
pub fn open_with(
    plain: &Path,
    mut open_file: impl FnMut(&Path) -> io::Result<File>,
) -> io::Result<Option<Source>> {
    for (path, compressed) in attempts(plain) {
        match open_file(&path) {
            Ok(file) => {
                let meta = file.metadata()?;
                return Ok(Some(Source {
                    file,
                    meta,
                    compressed,
                }));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(None)
}

/// Metadata of the file currently behind a plain path, in the same order as [`open`].
pub fn metadata(plain: &Path) -> io::Result<Metadata> {
    let [(first, _), rest @ ..] = attempts(plain);
    rest.into_iter()
        .fold(std::fs::metadata(first), |result, (path, _)| match result {
            Err(e) if e.kind() == io::ErrorKind::NotFound => std::fs::metadata(path),
            result => result,
        })
}

/// Codex publishes a rollout's new form before removing the old one, so after
/// the compressed form has gone missing the plain form exists again unless the
/// session itself was removed: one plain retry resolves a change in either
/// direction between two attempts.
fn attempts(plain: &Path) -> [(PathBuf, bool); 3] {
    [
        (plain.to_path_buf(), false),
        (compressed_path(plain), true),
        (plain.to_path_buf(), false),
    ]
}

impl Source {
    /// Decoded bytes from `offset`. A plain file is bounded to the length seen at
    /// open because its writer may still be appending; a compressed file is not
    /// written again, so it is read to its end, across any number of frames.
    pub fn records_from(mut self, offset: u64) -> io::Result<Box<dyn BufRead>> {
        if !self.compressed {
            self.file.seek(SeekFrom::Start(offset))?;
            let len = self.meta.len() - offset;
            return Ok(Box::new(BufReader::new(self.file.take(len))));
        }
        self.file.seek(SeekFrom::Start(0))?;
        let mut decoded = BufReader::new(zstd::stream::read::Decoder::new(self.file)?);
        skip(&mut decoded, offset)?;
        Ok(Box::new(decoded))
    }

    pub fn ranges(self) -> Ranges {
        Ranges {
            file: self.file,
            compressed: self.compressed,
            decoder: None,
        }
    }
}

type Decoder = BufReader<zstd::stream::read::Decoder<'static, BufReader<File>>>;

/// Reads record ranges of one source. A compressed source is decoded forward
/// once for ranges in increasing order; a range behind the decoder restarts it
/// from the first frame.
pub struct Ranges {
    file: File,
    compressed: bool,
    decoder: Option<(u64, Decoder)>,
}

impl Ranges {
    pub fn read_exact_at(&mut self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        if !self.compressed {
            return self.file.read_exact_at(buf, offset);
        }
        let (position, mut decoder) = match self.decoder.take() {
            Some((position, decoder)) if position <= offset => (position, decoder),
            _ => {
                let mut file = self.file.try_clone()?;
                file.seek(SeekFrom::Start(0))?;
                (0, BufReader::new(zstd::stream::read::Decoder::new(file)?))
            }
        };
        skip(&mut decoder, offset - position)?;
        decoder.read_exact(buf)?;
        self.decoder = Some((offset + buf.len() as u64, decoder));
        Ok(())
    }
}

fn skip(reader: &mut impl Read, count: u64) -> io::Result<()> {
    let skipped = io::copy(&mut reader.take(count), &mut io::sink())?;
    if skipped < count {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    Ok(())
}
