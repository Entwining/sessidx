use anyhow::Result;
use std::{
    fs::{File, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    time::{Duration, Instant},
};

pub struct WriterLock {
    file: File,
}
impl Drop for WriterLock {
    /// A child process spawned while this lock is held keeps a copy of its
    /// descriptor until it execs, and `flock` releases on close only when every
    /// copy is closed; an explicit unlock releases it at once.
    fn drop(&mut self) {
        // Closing the file still releases the lock once every copy is gone.
        let _ = self.file.unlock();
    }
}

impl WriterLock {
    /// Takes the lock, retrying every 10 ms until `deadline` when one is
    /// given, because `flock` has no timeout. Rule: docs/formats.md
    /// `writer_lock_wait`.
    pub(super) fn acquire(
        path: &Path,
        shared: bool,
        deadline: Option<Instant>,
    ) -> Result<Option<Self>> {
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path.with_extension("lock"))?;
        loop {
            match if shared {
                f.try_lock_shared()
            } else {
                f.try_lock()
            } {
                Ok(()) => return Ok(Some(Self { file: f })),
                Err(std::fs::TryLockError::WouldBlock) => {}
                Err(e) => return Err(e.into()),
            }
            let Some(left) = deadline
                .map(|d| d.saturating_duration_since(Instant::now()))
                .filter(|left| !left.is_zero())
            else {
                return Ok(None);
            };
            std::thread::sleep(left.min(Duration::from_millis(10)));
        }
    }
}
