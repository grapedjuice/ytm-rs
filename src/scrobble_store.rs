//! Account-owned scrobble queues and injectable credential storage.

use std::fs;
use std::io::{ErrorKind, Write};
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
    fn regular_file_exists(&self) -> anyhow::Result<bool> {
        match fs::symlink_metadata(&self.0) {
            Ok(m) => {
                anyhow::ensure!(m.is_file(), "the scrobble queue isn't a regular file");
                Ok(true)
            }
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    fn read<T: DeserializeOwned>(&self) -> anyhow::Result<Data<T>> {
        Ok(serde_json::from_slice(&fs::read(&self.0)?)?)
    }

    /// Only credentials recovered at startup may identify a legacy queue's owner.
    pub fn restore<T: Serialize + DeserializeOwned>(&self, owner: Option<&str>) -> anyhow::Result<Vec<T>> {
        let Some(owner) = owner else { return Ok(Vec::new()) };
        if !self.regular_file_exists()? {
            return Ok(Vec::new());
        }
        match self.read()? {
            Data::Owned(q) => {
                anyhow::ensure!(q.version == 1, "unsupported scrobble queue version {}", q.version);
                Ok(if q.owner == owner { q.items } else { Vec::new() })
            }
            Data::Legacy(items) => {
                if let Err(e) = self.write(owner, &items) {
                    log::warn!("couldn't migrate scrobble queue at {}: {e:#}", self.0.display());
                }
                Ok(items)
            }
        }
    }

    /// Discard queues that cannot be attributed to the account signing in.
    pub fn prepare<T: DeserializeOwned>(&self, owner: &str) -> anyhow::Result<Vec<T>> {
        if !self.regular_file_exists()? {
            return Ok(Vec::new());
        }
        let reason = match self.read::<T>() {
            Ok(Data::Owned(q)) if q.version != 1 => format!("unsupported version {}", q.version),
            Ok(Data::Owned(q)) if q.owner == owner => return Ok(q.items),
            Ok(_) => "no matching account".to_owned(),
            Err(e) => format!("couldn't read queue: {e:#}"),
        };
        self.remove()?;
        log::warn!("discarded scrobble queue at {}: {reason}", self.0.display());
        Ok(Vec::new())
    }

    fn remove(&self) -> anyhow::Result<()> {
        if self.regular_file_exists()? {
            fs::remove_file(&self.0)?;
        }
        Ok(())
    }

    pub fn save<T: Serialize>(&self, owner: &str, items: &[T]) -> anyhow::Result<()> {
        if items.is_empty() {
            return self.remove();
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
            let temp = self.temp_path();
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = match options.open(&temp) {
                Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
                result => result?,
            };
            let result = (|| {
                file.write_all(&bytes)?;
                file.sync_all()
            })();
            drop(file);
            let result = result.and_then(|()| fs::rename(&temp, &self.0));
            if result.is_err() {
                let _ = fs::remove_file(&temp);
            }
            return Ok(result?);
        }
    }

    fn temp_path(&self) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
        self.0.with_file_name(format!("{}.tmp-{}-{at}-{}", self.0.file_name().unwrap().to_string_lossy(), std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scrobble_tests::Profile;

    #[test]
    fn sign_in_discards_foreign_unknown_and_malformed_queues() {
        let profile = Profile::new();
        let file = QueueFile(profile.0.join("queue.json"));
        for bytes in [r#"{"version":1,"owner":"bob","items":[2]}"#, r#"{"version":2,"owner":"alice","items":[1]}"#, "broken json"] {
            fs::write(&file.0, bytes).unwrap();
            assert!(file.prepare::<u32>("alice").unwrap().is_empty());
            assert!(!file.0.exists());
        }
        assert_eq!(fs::read_dir(&profile.0).unwrap().count(), 0);
    }

    #[test]
    fn replacement_is_complete_and_leaves_no_temporary_files() {
        let profile = Profile::new();
        let file = QueueFile(profile.0.join("queue.json"));
        file.save("alice", &[1_u32, 2]).unwrap();
        file.save("alice", &[3_u32]).unwrap();
        assert_eq!(file.prepare::<u32>("alice").unwrap(), [3]);
        assert_eq!(fs::read_dir(&profile.0).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&file.0).unwrap().permissions().mode() & 0o777, 0o600);
        }
        file.save::<u32>("alice", &[]).unwrap();
        assert!(!file.0.exists());
    }
}
