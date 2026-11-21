//! Account-owned scrobble queues and injectable credential storage.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub trait Credentials: Send + Sync {
    fn get(&self, name: &str, dir: &Path) -> Option<Vec<u8>>;
    fn set(&self, name: &str, dir: &Path, secret: &[u8]) -> anyhow::Result<()>;
    fn delete(&self, name: &str, dir: &Path);
}

pub struct Vault;

impl Credentials for Vault {
    fn get(&self, name: &str, dir: &Path) -> Option<Vec<u8>> {
        crate::vault::get(name, dir)
    }
    fn set(&self, name: &str, dir: &Path, secret: &[u8]) -> anyhow::Result<()> {
        crate::vault::set(name, dir, secret)
    }
    fn delete(&self, name: &str, dir: &Path) {
        crate::vault::delete(name, dir);
    }
}

#[derive(Serialize, Deserialize)]
struct Saved<T> {
    version: u32,
    owner: String,
    items: Vec<T>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Data<T> {
    Owned(Saved<T>),
    Legacy(Vec<T>),
}

pub struct QueueFile(pub PathBuf);

impl QueueFile {
    fn read<T: DeserializeOwned>(&self) -> anyhow::Result<Option<Data<T>>> {
        match std::fs::read(&self.0) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes).ok()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Only credentials recovered at startup may identify a legacy queue's owner.
    pub fn restore<T: Serialize + DeserializeOwned>(&self, owner: Option<&str>) -> anyhow::Result<Vec<T>> {
        match (self.read()?, owner) {
            (Some(Data::Owned(q)), Some(owner)) if q.version == 1 && q.owner == owner => Ok(q.items),
            (Some(Data::Legacy(items)), Some(owner)) if !owner.trim().is_empty() => {
                self.write(owner, &items)?;
                Ok(items)
            }
            _ => Ok(Vec::new()),
        }
    }

    /// A newly entered login cannot identify an orphaned legacy queue. Move any
    /// unmatched or unreadable format out of the active slot before changing credentials.
    pub fn prepare<T: DeserializeOwned>(&self, owner: &str) -> anyhow::Result<Vec<T>> {
        anyhow::ensure!(!owner.trim().is_empty(), "the scrobble account needs a name");
        if let Some(Data::Owned(q)) = self.read()?
            && q.version == 1
            && q.owner == owner
        {
            return Ok(q.items);
        }
        match std::fs::symlink_metadata(&self.0) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
            Ok(m) => anyhow::ensure!(m.is_file(), "the scrobble queue isn't a regular file"),
        }
        // Reserve a new directory exclusively; its queue.json cannot collide with
        // another backup. Rename within the same filesystem preserves the original bytes.
        loop {
            let dir = self.sibling("unassigned");
            let result = {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::DirBuilderExt;
                    let mut builder = std::fs::DirBuilder::new();
                    builder.mode(0o700);
                    builder.create(&dir)
                }
                #[cfg(not(unix))]
                {
                    std::fs::create_dir(&dir)
                }
            };
            match result {
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
                Ok(()) => {}
            }
            let backup = dir.join("queue.json");
            if let Err(e) = std::fs::rename(&self.0, &backup) {
                let _ = std::fs::remove_dir(&dir);
                return Err(e.into());
            }
            log::warn!("scrobble queue has no matching account; preserved at {}", backup.display());
            return Ok(Vec::new());
        }
    }

    pub fn save<T: Serialize + DeserializeOwned>(&self, owner: &str, items: &[T]) -> anyhow::Result<()> {
        self.prepare::<T>(owner)?;
        if items.is_empty() {
            return match std::fs::remove_file(&self.0) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                result => Ok(result?),
            };
        }
        self.write(owner, items)
    }

    fn write<T: Serialize>(&self, owner: &str, items: &[T]) -> anyhow::Result<()> {
        #[derive(Serialize)]
        struct Output<'a, T> {
            version: u32,
            owner: &'a str,
            items: &'a [T],
        }
        let bytes = serde_json::to_vec(&Output { version: 1, owner, items })?;
        loop {
            let temp = self.sibling("tmp");
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = match options.open(&temp) {
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                result => result?,
            };
            let result = (|| {
                file.write_all(&bytes)?;
                file.sync_all()
            })();
            drop(file);
            let result = result.and_then(|()| std::fs::rename(&temp, &self.0));
            if result.is_err() {
                let _ = std::fs::remove_file(&temp);
            }
            return Ok(result?);
        }
    }

    fn sibling(&self, kind: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
        self.0.with_file_name(format!("{}.{}-{}-{at}-{}", self.0.file_name().unwrap().to_string_lossy(), kind, std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)))
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::sync::{Mutex, atomic::AtomicBool};

    pub struct MemoryCredentials {
        secret: Mutex<Option<Vec<u8>>>,
        pub unavailable: AtomicBool,
        pub reject_writes: AtomicBool,
    }

    impl Default for MemoryCredentials {
        fn default() -> Self {
            Self { secret: Mutex::new(None), unavailable: AtomicBool::new(false), reject_writes: AtomicBool::new(false) }
        }
    }

    impl MemoryCredentials {
        pub fn value(&self) -> Option<Vec<u8>> {
            self.secret.lock().unwrap().clone()
        }
    }

    impl Credentials for MemoryCredentials {
        fn get(&self, _: &str, _: &Path) -> Option<Vec<u8>> {
            if self.unavailable.load(Ordering::Relaxed) { None } else { self.value() }
        }
        fn set(&self, _: &str, _: &Path, secret: &[u8]) -> anyhow::Result<()> {
            anyhow::ensure!(!self.reject_writes.load(Ordering::Relaxed), "test credential write failed");
            *self.secret.lock().unwrap() = Some(secret.to_vec());
            Ok(())
        }
        fn delete(&self, _: &str, _: &Path) {
            *self.secret.lock().unwrap() = None;
        }
    }

    pub struct Profile {
        pub path: PathBuf,
    }

    impl Profile {
        pub fn new(service: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "ytm-{service}-queue-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self { path: path.canonicalize().unwrap() }
        }

        pub fn preserved(&self) -> Vec<Vec<u8>> {
            std::fs::read_dir(&self.path)
                .unwrap()
                .map(Result::unwrap)
                .filter(|e| e.file_name().to_string_lossy().contains(".unassigned-"))
                .map(|e| std::fs::read(e.path().join("queue.json")).unwrap())
                .collect()
        }
    }

    impl Drop for Profile {
        fn drop(&mut self) {
            // Only remove the exclusively created direct child of the system temp directory.
            let temp = std::env::temp_dir().canonicalize().unwrap();
            if let Ok(path) = self.path.canonicalize() {
                assert_eq!(path.parent(), Some(temp.as_path()));
                assert!(path.file_name().unwrap().to_string_lossy().starts_with("ytm-"));
                let _ = std::fs::remove_dir_all(path);
            }
        }
    }

    #[test]
    fn unknown_formats_and_mismatched_owners_are_preserved_byte_for_byte() {
        for bytes in [b"broken json".to_vec(), br#"{"version":2,"owner":"alice","items":[1]}"#.to_vec(), br#"{"version":1,"owner":"bob","items":[2]}"#.to_vec(), b"[3]".to_vec()] {
            let p = Profile::new("store");
            let file = QueueFile(p.path.join("queue.json"));
            std::fs::write(&file.0, &bytes).unwrap();
            assert!(file.restore::<u32>(None).unwrap().is_empty());
            assert_eq!(std::fs::read(&file.0).unwrap(), bytes);
            assert!(file.prepare::<u32>("alice").unwrap().is_empty());
            assert_eq!(p.preserved(), vec![bytes]);
            file.save("alice", &[4_u32]).unwrap();
            assert_eq!(file.restore::<u32>(Some("alice")).unwrap(), vec![4]);
            assert!(file.restore::<u32>(Some("bob")).unwrap().is_empty());
        }
    }

    #[test]
    fn replacement_is_complete_and_leaves_no_temporary_files() {
        let p = Profile::new("store");
        let file = QueueFile(p.path.join("queue.json"));
        file.save("alice", &[1_u32, 2]).unwrap();
        file.save("alice", &[3_u32]).unwrap();
        assert_eq!(file.restore::<u32>(Some("alice")).unwrap(), vec![3]);
        assert_eq!(std::fs::read_dir(&p.path).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&file.0).unwrap().permissions().mode() & 0o777, 0o600);
        }
        file.save::<u32>("alice", &[]).unwrap();
        assert!(!file.0.exists());
    }
}
