use std::path::{Path, PathBuf};
use std::sync::{Mutex, atomic::{AtomicU64, Ordering}};

use crate::scrobble_store::Credentials;

#[derive(Default)]
pub(super) struct MemoryCredentials {
    secret: Mutex<Option<Vec<u8>>>,
    after_set: Option<fn(&Path)>,
}

impl MemoryCredentials {
    pub(super) fn checking_set(check: fn(&Path)) -> Self {
        Self { after_set: Some(check), ..Self::default() }
    }
}

impl Credentials for MemoryCredentials {
    fn get(&self, _: &str, _: &Path) -> Option<Vec<u8>> {
        self.secret.lock().unwrap().clone()
    }
    fn set(&self, _: &str, dir: &Path, secret: &[u8]) -> anyhow::Result<()> {
        *self.secret.lock().unwrap() = Some(secret.to_vec());
        if let Some(check) = self.after_set {
            check(dir);
        }
        Ok(())
    }
    fn delete(&self, _: &str, _: &Path) {
        *self.secret.lock().unwrap() = None;
    }
}

pub(super) struct Profile(pub PathBuf);

impl Profile {
    pub(super) fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!("ytm-scrobble-{}-{at}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        let path = self.0.canonicalize().unwrap();
        assert_eq!(path, self.0);
        assert_eq!(path.parent(), Some(std::env::temp_dir().canonicalize().unwrap().as_path()));
        std::fs::remove_dir_all(path).unwrap();
    }
}

// Expand inside each service so the tests can exercise private session and transport boundaries.
macro_rules! service_tests {
    ($send:ident, $secret:ident, $file:literal, $revoked:literal, $ok:expr, $drops_400:literal, $item:ident { $($extra:tt)* }) => {
        use super::*;
        use crate::scrobble_tests::{MemoryCredentials, Profile};
        use std::{fs, future::ready, task::{Context, Waker}};

        fn item(track: &str) -> $item {
            $item { artist: "Artist".into(), track: track.into(), album: None, duration: None, timestamp: 1, $($extra)* }
        }

        fn login(st: &mut State, name: &str) {
            let session = Session { name: name.into(), $secret: "secret".into() };
            let queue = save_session(&st.data_dir, st.credentials.as_ref(), &session).unwrap();
            use_session(st, session, queue);
        }

        #[tokio::test]
        async fn stale_responses_leave_sign_out_and_replacement_sessions_intact() {
            for name in [None, Some("alice"), Some("bob")] {
                for code in [0, $revoked] {
                    let profile = Profile::new();
                    let credentials = Arc::new(MemoryCredentials::default());
                    let mut st = load_state(&profile.0, credentials.clone());
                    login(&mut st, "alice");
                    let state = Mutex::new(st);
                    let sending = tokio::sync::Mutex::new(());
                    $send(&state, &sending, item("old"), |batch, original| {
                        assert_eq!(batch, [item("old")]);
                        let mut st = state.lock().unwrap();
                        assert!(is_current(&st, &original));
                        disconnect(&mut st);
                        if let Some(name) = name {
                            login(&mut st, name);
                            st.queue.push(item("replacement"));
                            persist_queue(&st);
                        }
                        assert!(!is_current(&st, &original));
                        ready(if code == 0 { Ok($ok) } else { Err(ApiError(code, "revoked".into()).into()) })
                    }).await;
                    let st = state.into_inner().unwrap();
                    let restored = load_state(&profile.0, credentials);
                    let expected = if name.is_some() { vec![item("replacement")] } else { vec![] };
                    for st in [st, restored] {
                        assert_eq!(st.session.as_ref().map(|s| s.name.as_str()), name);
                        assert_eq!(st.queue, expected);
                    }
                }
            }
        }

        #[tokio::test]
        async fn work_captured_before_spawn_or_waiting_on_send_cannot_change_sessions() {
            for poll_before_reconnect in [false, true] {
                let profile = Profile::new();
                let mut st = load_state(&profile.0, Arc::new(MemoryCredentials::default()));
                login(&mut st, "alice");
                let state = Mutex::new(st);
                let sending = tokio::sync::Mutex::new(());
                let lock = sending.lock().await;
                let pending = $send(&state, &sending, item("old"), |_, _| async { panic!("stale submission") });
                let now_playing = now_playing_with(&state, |_| async { panic!("stale now playing") });
                tokio::pin!(pending);
                if poll_before_reconnect {
                    assert!(pending.as_mut().poll(&mut Context::from_waker(Waker::noop())).is_pending());
                    assert_eq!(state.lock().unwrap().queue, [item("old")]);
                }
                {
                    let mut st = state.lock().unwrap();
                    login(&mut st, "bob");
                    st.queue.push(item("replacement"));
                    persist_queue(&st);
                }
                drop(lock);
                pending.await;
                now_playing.await;
                assert_eq!(state.lock().unwrap().queue, [item("replacement")]);
            }
        }

        #[test]
        fn only_startup_credentials_can_claim_a_legacy_queue() {
            let profile = Profile::new();
            let path = profile.0.join($file);
            let legacy = serde_json::to_vec(&[item("legacy")]).unwrap();
            let credentials = Arc::new(MemoryCredentials::default());
            credentials.set(VAULT, &profile.0, b"alice\nsecret").unwrap();
            fs::write(&path, &legacy).unwrap();
            assert_eq!(load_state(&profile.0, credentials).queue, [item("legacy")]);
            assert!(QueueFile(path.clone()).restore::<$item>(Some("bob")).unwrap().is_empty());

            fs::write(&path, legacy).unwrap();
            let credentials = Arc::new(MemoryCredentials::checking_set(|dir| {
                assert!(!dir.join($file).exists(), "a kill here would expose the legacy queue");
            }));
            let mut st = load_state(&profile.0, credentials.clone());
            assert!(st.session.is_none() && st.queue.is_empty());
            login(&mut st, "bob");
            let restarted = load_state(&profile.0, credentials);
            assert_eq!(restarted.session.unwrap().name, "bob");
            assert!(restarted.queue.is_empty());
        }

        #[tokio::test]
        async fn disk_failure_keeps_the_connection_and_next_save_catches_up() {
            let profile = Profile::new();
            let path = profile.0.join($file);
            let credentials = Arc::new(MemoryCredentials::default());
            credentials.set(VAULT, &profile.0, b"alice\nsecret").unwrap();
            fs::create_dir(&path).unwrap();
            let st = load_state(&profile.0, credentials.clone());
            assert!(st.session.is_some() && st.queue.is_empty());
            let state = Mutex::new(st);
            let sending = tokio::sync::Mutex::new(());
            $send(&state, &sending, item("unsaved"), |batch, _| {
                assert_eq!(batch, [item("unsaved")]);
                ready(Err(anyhow::anyhow!("offline")))
            }).await;
            fs::remove_dir(&path).unwrap();
            let retained = [item("unsaved"), item("next")];
            $send(&state, &sending, item("next"), |batch, _| {
                assert_eq!(batch, retained);
                assert_eq!(load_state(&profile.0, credentials.clone()).queue, retained);
                ready(Err(ApiError(400, "bad request".into()).into()))
            }).await;
            let restored = load_state(&profile.0, credentials);
            let expected: &[$item] = if $drops_400 { &[] } else { &retained };
            assert!(restored.session.is_some());
            assert_eq!(restored.queue, expected);
            assert_eq!(state.into_inner().unwrap().queue, expected);
        }
    };
}

pub(super) use service_tests;
