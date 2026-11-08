use hyperlight_unikraft::{DirBackend, DirEntry, FileStat, FsBackend, Mount, SandboxBuilder};
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

const BUDGET: u64 = 512 * 1024 * 1024;

struct Storage {
    directory: DirBackend,
    audit: Mutex<std::fs::File>,
    bytes: Mutex<u64>,
}

impl Storage {
    fn record<T>(&self, operation: &str, path: &str, value: io::Result<T>) -> io::Result<T> {
        let error = value.as_ref().err().map(|e| format!("{e:?}"));
        let mut log = self
            .audit
            .lock()
            .map_err(|_| io::Error::other("audit lock poisoned"))?;
        serde_json::to_writer(
            &mut *log,
            &serde_json::json!({
                "operation": operation, "path": path, "ok": value.is_ok(), "error": error
            }),
        )?;
        log.write_all(b"\n")?;
        log.flush()?;
        value
    }

    fn size(&self, path: &str) -> io::Result<u64> {
        match self.directory.stat(path) {
            Ok(st) => Ok(
                if st.mode & hyperlight_unikraft::S_IFMT == hyperlight_unikraft::S_IFDIR {
                    0
                } else {
                    st.size
                },
            ),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoSync(DirBackend);
    impl FsBackend for NoSync {
        fn stat(&self, p: &str) -> io::Result<FileStat> {
            self.0.stat(p)
        }
        fn read(&self, p: &str, o: u64, b: &mut [u8]) -> io::Result<usize> {
            self.0.read(p, o, b)
        }
        fn write(&self, p: &str, o: Option<u64>, b: &[u8]) -> io::Result<()> {
            self.0.write(p, o, b)
        }
        fn create_dir(&self, p: &str) -> io::Result<()> {
            self.0.create_dir(p)
        }
        fn remove(&self, p: &str) -> io::Result<()> {
            self.0.remove(p)
        }
        fn truncate(&self, p: &str, s: u64) -> io::Result<()> {
            self.0.truncate(p, s)
        }
        fn list(&self, p: &str) -> io::Result<Vec<DirEntry>> {
            self.0.list(p)
        }
        fn rename(&self, p: &str, q: &str) -> io::Result<()> {
            self.0.rename(p, q)
        }
    }

    #[test]
    fn absent_sync_and_pagination_fail_explicitly() {
        let root = tempfile::tempdir().unwrap();
        let backend = NoSync(DirBackend::open(root.path()).unwrap());
        assert_eq!(
            backend.sync("", false).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(
            backend.list_page("", "", 128).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
    }

    #[test]
    fn syncs_real_files_directories_and_reports_errors() {
        let root = tempfile::tempdir().unwrap();
        let directory = DirBackend::open(root.path()).unwrap();
        directory.write("file", Some(0), b"abc").unwrap();
        directory.sync("file", false).unwrap();
        directory.sync("file", true).unwrap();
        directory.sync("", false).unwrap();
        assert_eq!(
            directory.sync("missing", false).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert!(directory.sync("../outside", false).is_err());
        assert!(directory.list_page("../", "", 128).is_err());
    }

    #[test]
    fn pages_exceed_old_transport_limit_in_stable_order() {
        let root = tempfile::tempdir().unwrap();
        for i in 0..5000 {
            std::fs::write(root.path().join(format!("entry-{i:04}")), "").unwrap();
        }
        let directory = DirBackend::open(root.path()).unwrap();
        let mut after = String::new();
        let mut count = 0;
        loop {
            let page = directory.list_page("", &after, 128).unwrap();
            if page.is_empty() {
                break;
            }
            for entry in page {
                assert_eq!(entry.name, format!("entry-{count:04}"));
                after = entry.name;
                count += 1;
            }
        }
        assert_eq!(count, 5000);
    }

    #[test]
    fn quota_and_partial_write_failures_are_not_success() {
        let root = tempfile::tempdir().unwrap();
        let log = tempfile::tempfile().unwrap();
        std::fs::create_dir(root.path().join("probes")).unwrap();
        let backend = Storage {
            directory: DirBackend::open(root.path()).unwrap(),
            audit: Mutex::new(log),
            bytes: Mutex::new(0),
        };
        assert!(backend.write("too-large", Some(BUDGET), b"x").is_err());
        assert!(!root.path().join("too-large").exists());
        assert!(
            backend
                .write("probes/partial-write", Some(0), b"123456789")
                .is_err()
        );
        assert_eq!(
            std::fs::read(root.path().join("probes/partial-write")).unwrap(),
            b"1234567"
        );
        assert_eq!(*backend.bytes.lock().unwrap(), 7);
        backend.remove("probes/partial-write").unwrap();
        assert_eq!(*backend.bytes.lock().unwrap(), 0);
    }
}

impl Storage {
    fn change(
        &self,
        path: &str,
        new_size: u64,
        op: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        let mut bytes = self
            .bytes
            .lock()
            .map_err(|_| io::Error::other("budget lock poisoned"))?;
        let old_size = self.size(path)?;
        let projected = bytes
            .checked_sub(old_size)
            .and_then(|b| b.checked_add(new_size))
            .ok_or_else(|| io::Error::other("invalid storage accounting"))?;
        if projected > BUDGET {
            return Err(io::ErrorKind::QuotaExceeded.into());
        }
        let result = op();
        let actual = self.size(path)?;
        *bytes = bytes
            .checked_sub(old_size)
            .and_then(|b| b.checked_add(actual))
            .ok_or_else(|| io::Error::other("invalid storage accounting"))?;
        result
    }
}

impl FsBackend for Storage {
    fn stat(&self, path: &str) -> io::Result<FileStat> {
        self.directory.stat(path)
    }
    fn read(&self, path: &str, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        self.directory.read(path, offset, buf)
    }
    fn write(&self, path: &str, offset: Option<u64>, data: &[u8]) -> io::Result<()> {
        let old = self.size(path)?;
        let start = offset.unwrap_or(old);
        let end = start
            .checked_add(data.len() as u64)
            .ok_or_else(|| io::Error::from(io::ErrorKind::FileTooLarge))?;
        let result = self.change(path, end.max(old), || {
            if path == "probes/partial-write" && !data.is_empty() {
                self.directory
                    .write(path, offset, &data[..data.len().min(7)])?;
                return Err(io::Error::from_raw_os_error(5));
            }
            self.directory.write(path, offset, data)
        });
        self.record("write", path, result)
    }
    fn create_dir(&self, path: &str) -> io::Result<()> {
        self.record("mkdir", path, self.directory.create_dir(path))
    }
    fn remove(&self, path: &str) -> io::Result<()> {
        let old = self.size(path)?;
        let result = self.directory.remove(path);
        if result.is_ok() {
            let mut bytes = self
                .bytes
                .lock()
                .map_err(|_| io::Error::other("budget lock poisoned"))?;
            *bytes = bytes
                .checked_sub(old)
                .ok_or_else(|| io::Error::other("budget underflow"))?;
        }
        self.record("remove", path, result)
    }
    fn truncate(&self, path: &str, len: u64) -> io::Result<()> {
        self.record(
            "truncate",
            path,
            self.change(path, len, || self.directory.truncate(path, len)),
        )
    }
    fn list(&self, path: &str) -> io::Result<Vec<DirEntry>> {
        self.directory.list(path)
    }
    fn list_page(&self, path: &str, after: &str, limit: usize) -> io::Result<Vec<DirEntry>> {
        self.record(
            "list_page",
            path,
            self.directory.list_page(path, after, limit),
        )
    }
    fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        let replaced = self.size(to)?;
        let result = self.directory.rename(from, to);
        if result.is_ok() {
            let mut bytes = self
                .bytes
                .lock()
                .map_err(|_| io::Error::other("budget lock poisoned"))?;
            *bytes = bytes
                .checked_sub(replaced)
                .ok_or_else(|| io::Error::other("budget underflow"))?;
        }
        self.record("rename", from, result)
    }
    fn chmod(&self, path: &str, mode: u32) -> io::Result<()> {
        self.record("chmod", path, self.directory.chmod(path, mode))
    }
    fn set_times(
        &self,
        path: &str,
        accessed: Option<SystemTime>,
        modified: Option<SystemTime>,
    ) -> io::Result<()> {
        self.directory.set_times(path, accessed, modified)
    }
    fn sync(&self, path: &str, data_only: bool) -> io::Result<()> {
        let result = if path == "probes/fail-sync" || path == "probes/fail-dir-sync" {
            Err(io::Error::from_raw_os_error(5))
        } else {
            self.directory.sync(path, data_only)
        };
        self.record(
            if data_only { "sync_data" } else { "sync_all" },
            path,
            result,
        )
    }
}

fn bytes_in(root: &Path) -> io::Result<u64> {
    let mut bytes = 0;
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        if meta.is_dir() {
            bytes += bytes_in(&entry.path())?;
        } else {
            bytes += meta.len();
        }
    }
    Ok(bytes)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 6 {
        return Err("usage: postgres-storage ROOTFS STORAGE AUDIT ENTRY VM_ID".into());
    }
    // Keep ownership in the VM host, not only its potentially short-lived launcher.
    let owner = std::fs::File::open(&args[2])?;
    owner.try_lock()?;
    let storage = Arc::new(Storage {
        directory: DirBackend::open(&args[2])?,
        audit: Mutex::new(
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&args[3])?,
        ),
        bytes: Mutex::new(bytes_in(Path::new(&args[2]))?),
    });
    if *storage.bytes.lock().map_err(|_| "budget lock poisoned")? > BUDGET {
        return Err("initial storage exceeds 512 MiB".into());
    }
    let mut guest = SandboxBuilder::from_initrd(&args[1])
        .scratch_mb(512)
        .entry(&args[4])
        .env("HL_PG_VM_ID", &args[5])
        .env("HL_PG_DATA", "/storage/pgdata")
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .listen_ports(hyperlight_unikraft::ListenPorts::from_ports([55432, 55433]))
        .mount(Mount::backend(storage.clone(), "/storage", false))
        .mount(Mount::backend(storage, "/readonly", true))
        .boot()?;
    let status = guest.join()?;
    if status != 0 {
        return Err(format!("guest exited {status}").into());
    }
    drop(owner);
    Ok(())
}
