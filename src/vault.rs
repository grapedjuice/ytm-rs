//! Secrets (the Google and Spotify sign-ins) in the OS credential store: Windows
//! Credential Manager, encrypted to the signed-in Windows user, on macOS the login
//! Keychain, or on Linux the Secret Service (GNOME Keyring, KWallet).
//!
//! A credential holds at most 2,560 bytes and a Google cookie is close to that, so a
//! secret is split over numbered entries: `name`, `name #2`, `name #3`, …
//! Linux without a Secret Service running, a Keychain that refuses, and other
//! systems keep the secret in a file only this user can read, in the data folder.

use std::path::Path;

const SERVICE: &str = "ytm-rs";

/// The credential names for `name`, scoped to the data folder when it isn't the
/// default one (`YTM_DATA_DIR` dev profiles, tests), so those never share or touch
/// the real sign-ins.
fn key(name: &str, data_dir: &Path) -> String {
    let default = directories::ProjectDirs::from("", "", "ytm-rs").map(|d| d.data_dir().to_path_buf());
    if default.as_deref() == Some(data_dir) {
        name.to_owned()
    } else {
        format!("{name} ({})", data_dir.display())
    }
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
mod store {
    /// Credential Manager's limit on one secret.
    const CHUNK: usize = 2560;
    /// One store operation at a time: writes and deletes from several threads at once
    /// can be lost (stale parts were left behind in testing).
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[cfg(all(test, windows))]
    pub fn tests_entry(key: &str, part: usize) -> keyring::Entry {
        entry(key, part).unwrap()
    }

    fn entry(key: &str, part: usize) -> keyring::Result<keyring::Entry> {
        let user = if part == 0 { key.to_owned() } else { format!("{key} #{}", part + 1) };
        keyring::Entry::new(super::SERVICE, &user)
    }

    pub fn get(key: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let _one = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = Vec::new();
        for part in 0.. {
            match entry(key, part)?.get_secret() {
                Ok(bytes) => {
                    let full = bytes.len() == CHUNK;
                    out.extend(bytes);
                    if !full {
                        break;
                    }
                }
                Err(keyring::Error::NoEntry) if part == 0 => return Ok(None),
                Err(keyring::Error::NoEntry) => break,
                Err(e) => return Err(e.into()),
            }
        }
        Ok(Some(out))
    }

    pub fn set(key: &str, secret: &[u8]) -> anyhow::Result<()> {
        let _one = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // A secret of an exact multiple of CHUNK ends with an empty part, so reading knows where it stops.
        let mut parts: Vec<&[u8]> = secret.chunks(CHUNK).collect();
        if secret.len().is_multiple_of(CHUNK) {
            parts.push(&[]);
        }
        for (i, part) in parts.iter().enumerate() {
            entry(key, i)?.set_secret(part)?;
        }
        delete_from(key, parts.len())
    }

    pub fn delete(key: &str) -> anyhow::Result<()> {
        let _one = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        delete_from(key, 0)
    }

    /// Delete parts `first..` (left over from a longer secret).
    fn delete_from(key: &str, first: usize) -> anyhow::Result<()> {
        for part in first.. {
            match entry(key, part)?.delete_credential() {
                Ok(()) => {}
                Err(keyring::Error::NoEntry) => break,
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod file {
    use std::path::{Path, PathBuf};

    /// No store here: the secret is a file named after its key, in the data folder.
    fn path(key: &str, dir: &Path) -> PathBuf {
        let safe: String = key.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
        dir.join(format!("{safe}.secret"))
    }

    pub fn get(key: &str, dir: &Path) -> anyhow::Result<Option<Vec<u8>>> {
        match std::fs::read(path(key, dir)) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn set(key: &str, dir: &Path, secret: &[u8]) -> anyhow::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path(key, dir))?;
        Ok(f.write_all(secret)?)
    }

    pub fn delete(key: &str, dir: &Path) -> anyhow::Result<()> {
        match std::fs::remove_file(path(key, dir)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }
}

/// The store everywhere but Windows: the Keychain on macOS or the Secret Service on
/// Linux when one answers, otherwise the file in `dir`.
#[cfg(not(windows))]
mod either {
    use std::path::Path;

    pub fn get(key: &str, dir: &Path) -> anyhow::Result<Option<Vec<u8>>> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        match super::store::get(key) {
            Ok(Some(s)) => return Ok(Some(s)),
            Ok(None) => {}
            Err(e) => log::debug!("credential store unavailable: {e:#}"),
        }
        super::file::get(key, dir)
    }

    pub fn set(key: &str, dir: &Path, secret: &[u8]) -> anyhow::Result<()> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        match super::store::set(key, secret) {
            // Saved in the keyring: drop any copy an earlier run left in a file.
            Ok(()) => return super::file::delete(key, dir),
            Err(e) => log::info!("no credential store, keeping {key:?} in a file: {e:#}"),
        }
        super::file::set(key, dir, secret)
    }

    pub fn delete(key: &str, dir: &Path) -> anyhow::Result<()> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if let Err(e) = super::store::delete(key) {
            log::debug!("credential store unavailable: {e:#}");
        }
        super::file::delete(key, dir)
    }
}

#[cfg(not(windows))]
use either as backend;

/// Windows has only the Credential Manager; the data folder is just part of the key.
#[cfg(windows)]
mod backend {
    use std::path::Path;

    pub fn get(key: &str, _dir: &Path) -> anyhow::Result<Option<Vec<u8>>> {
        super::store::get(key)
    }
    pub fn set(key: &str, _dir: &Path, secret: &[u8]) -> anyhow::Result<()> {
        super::store::set(key, secret)
    }
    pub fn delete(key: &str, _dir: &Path) -> anyhow::Result<()> {
        super::store::delete(key)
    }
}

/// The secret saved under `name`, if any.
pub fn get(name: &str, data_dir: &Path) -> Option<Vec<u8>> {
    backend::get(&key(name, data_dir), data_dir).unwrap_or_else(|e| {
        log::warn!("couldn't read {name} from the credential store: {e:#}");
        None
    })
}

pub fn set(name: &str, data_dir: &Path, secret: &[u8]) -> anyhow::Result<()> {
    backend::set(&key(name, data_dir), data_dir, secret)
}

pub fn delete(name: &str, data_dir: &Path) {
    if let Err(e) = backend::delete(&key(name, data_dir), data_dir) {
        log::warn!("couldn't delete {name} from the credential store: {e:#}");
    }
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn round_trip_across_chunks() {
        let key = &format!("ytm-rs vault test {}", std::process::id());
        for len in [0, 10, 2560, 2561, 6000, 100] {
            let secret: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            super::store::set(key, &secret).unwrap();
            assert_eq!(super::store::get(key).unwrap().unwrap(), secret, "{len} bytes");
        }
        super::store::delete(key).unwrap();
        assert!(super::store::get(key).unwrap().is_none());
    }
}

#[cfg(all(test, windows))]
mod stress {
    #[test]
    #[ignore]
    fn parallel_stress() {
        let pid = std::process::id();
        let handles: Vec<_> = (0..4)
            .map(|t| {
                std::thread::spawn(move || {
                    for round in 0..15 {
                        let key = format!("ytm-rs stress {pid} {t}");
                        super::store::set(&key, &vec![7u8; 6000]).unwrap();
                        super::store::set(&key, &[7u8; 100]).unwrap();
                        assert_eq!(super::store::get(&key).unwrap().unwrap().len(), 100, "round {round}");
                        super::store::delete(&key).unwrap();
                        for part in 0..3 {
                            assert!(matches!(super::store::tests_entry(&key, part).get_secret(), Err(keyring::Error::NoEntry)), "thread {t} round {round} part {part} left");
                        }
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod unix_tests {
    #[test]
    fn round_trip_in_keyring_or_file() {
        let dir = std::env::temp_dir().join(format!("ytm-rs-vault-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for len in [0, 10, 2560, 2561, 6000, 100] {
            let secret: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            super::set("test", &dir, &secret).unwrap();
            assert_eq!(super::get("test", &dir).unwrap(), secret, "{len} bytes");
        }
        let in_keyring = super::store::get(&super::key("test", &dir)).ok().flatten().is_some();
        let files: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).collect();
        // Exactly one copy: in the keyring, or in an owner-only file.
        if in_keyring {
            assert!(files.is_empty(), "keyring secret also left in a file");
        } else {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(files.len(), 1);
            assert_eq!(files[0].metadata().unwrap().permissions().mode() & 0o777, 0o600);
        }
        eprintln!("stored in {}", if in_keyring { "the credential store" } else { "a file" });
        super::delete("test", &dir);
        assert!(super::get("test", &dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
