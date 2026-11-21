use super::*;
use crate::scrobble_store::tests::{MemoryCredentials, Profile};
use std::future::{poll_fn, ready};
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::task::Poll;
use tokio::sync::oneshot;

type Response = anyhow::Result<Value>;
type Transport = Box<dyn FnMut(Vec<Scrobble>, Arc<Session>) -> Pin<Box<dyn Future<Output = Response> + Send>> + Send>;
const WAIT: Duration = Duration::from_secs(5);

fn item(track: &str) -> Scrobble {
    Scrobble { artist: "Test artist".into(), track: track.into(), album: None, duration: Some(120), timestamp: 1_700_000_000 }
}

fn response(code: u64) -> Response {
    if code == 0 { Ok(serde_json::json!({ "scrobbles": { "@attr": { "accepted": 1 } } })) } else { Err(ApiError(code, "test response".into()).into()) }
}

struct Fixture {
    state: Mutex<State>,
    sending: tokio::sync::Mutex<()>,
    credentials: Arc<MemoryCredentials>,
    replacement: Mutex<Vec<Scrobble>>,
    profile: Profile,
}

impl Fixture {
    fn new() -> Self {
        let profile = Profile::new("lastfm");
        let credentials = Arc::new(MemoryCredentials::default());
        let fixture = Self { state: Mutex::new(load_state(&profile.path, credentials.clone())), sending: tokio::sync::Mutex::new(()), credentials, replacement: Mutex::new(Vec::new()), profile };
        {
            let mut st = fixture.state.lock().unwrap();
            keep_session(&mut st, Session { name: "old user".into(), key: "old secret".into() }).unwrap();
            st.queue.push(item("old backlog"));
            save_queue(&st).unwrap();
        }
        fixture
    }

    fn replace(&self, same_credentials: bool, sign_out_first: bool) -> Arc<Session> {
        let mut st = self.state.lock().unwrap();
        if sign_out_first {
            disconnect(&mut st);
        }
        let (name, secret) = if same_credentials { ("old user", "old secret") } else { ("new user", "new secret") };
        keep_session(&mut st, Session { name: name.into(), key: secret.into() }).unwrap();
        if !same_credentials || sign_out_first {
            assert!(st.queue.is_empty(), "the backlog must not move to another account");
        }
        st.queue.push(item("new backlog"));
        save_queue(&st).unwrap();
        *self.replacement.lock().unwrap() = st.queue.clone();
        st.session.clone().unwrap()
    }

    fn assert_replacement(&self, session: &Arc<Session>) {
        let st = self.state.lock().unwrap();
        assert!(is_current(&st, session));
        assert_eq!(st.queue, *self.replacement.lock().unwrap());
        let disk: Vec<Scrobble> = QueueFile(st.data_dir.join("lastfm_queue.json")).restore(Some(&st.session.as_ref().unwrap().name)).unwrap();
        assert_eq!(disk, st.queue);
        assert_eq!(st.credentials.get(VAULT, &st.data_dir), Some(format!("{}\n{}", session.name, session.key).into_bytes()));
    }
}

fn delayed() -> (Transport, oneshot::Receiver<Vec<Scrobble>>, oneshot::Sender<Response>) {
    let (entered_tx, entered_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let mut once = Some((entered_tx, release_rx));
    let send: Transport = Box::new(move |batch, session| {
        assert_eq!(session.key, "old secret");
        let (entered, release) = once.take().expect("unexpected extra request from the old session");
        Box::pin(async move {
            entered.send(batch).unwrap();
            release.await.unwrap()
        })
    });
    (send, entered_rx, release_tx)
}

#[tokio::test]
async fn logout_during_success_leaves_a_usable_state_and_can_reconnect() {
    let f = Fixture::new();
    let (send, entered, release) = delayed();
    let pending = scrobble_with(&f.state, &f.sending, item("old in flight"), send);
    let logout = async {
        assert_eq!(entered.await.unwrap().len(), 2);
        disconnect(&mut f.state.lock().unwrap());
        release.send(response(0)).unwrap();
    };
    tokio::time::timeout(WAIT, async {
        tokio::join!(pending, logout);
    })
    .await
    .unwrap();
    {
        let st = f.state.lock().unwrap();
        assert!(st.session.is_none() && st.queue.is_empty());
        assert!(!st.data_dir.join("lastfm_queue.json").exists());
        assert!(st.credentials.get(VAULT, &st.data_dir).is_none());
    }
    let new = f.replace(false, false);
    scrobble_with(&f.state, &f.sending, item("new listen"), |_, _| ready(response(0))).await;
    let st = f.state.lock().unwrap();
    assert!(is_current(&st, &new));
    assert!(st.queue.is_empty());
}

#[tokio::test]
async fn late_responses_cannot_modify_replaced_sessions_even_with_identical_credentials() {
    for code in [0, 9, 11] {
        for same_credentials in [false, true] {
            for sign_out_first in [false, true] {
                let f = Fixture::new();
                let (send, entered, release) = delayed();
                let pending = scrobble_with(&f.state, &f.sending, item("old in flight"), send);
                let replacement = async {
                    assert_eq!(entered.await.unwrap().len(), 2);
                    let new = f.replace(same_credentials, sign_out_first);
                    release.send(response(code)).unwrap();
                    new
                };
                let (_, new) = tokio::time::timeout(WAIT, async { tokio::join!(pending, replacement) }).await.unwrap();
                f.assert_replacement(&new);
                // A new submission can still flush this account's backlog.
                let mut expected = f.replacement.lock().unwrap().clone();
                expected.push(item("new listen"));
                scrobble_with(&f.state, &f.sending, item("new listen"), |batch, session| {
                    assert!(Arc::ptr_eq(&session, &new));
                    assert_eq!(batch, expected);
                    ready(response(0))
                })
                .await;
                let st = f.state.lock().unwrap();
                assert!(is_current(&st, &new) && st.queue.is_empty());
                assert!(!st.data_dir.join("lastfm_queue.json").exists());
                assert_eq!(st.credentials.get(VAULT, &st.data_dir), Some(format!("{}\n{}", new.name, new.key).into_bytes()));
            }
        }
    }
}

#[tokio::test]
async fn an_unpolled_submission_keeps_its_original_session() {
    for signed_in in [false, true] {
        let f = Fixture::new();
        if !signed_in {
            disconnect(&mut f.state.lock().unwrap());
        }
        // Deliberately don't poll until after reconnecting. This is the same
        // synchronous factory used by the UI before handing the future to spawn.
        let pending = scrobble_with(&f.state, &f.sending, item("old scheduled"), |_, _| async { panic!("stale submission reached the transport") });
        let new = f.replace(false, true);
        pending.await;
        f.assert_replacement(&new);
    }
}

#[tokio::test]
async fn a_submission_waiting_for_the_send_lock_cannot_join_a_new_session() {
    let f = Fixture::new();
    let blocked = f.sending.lock().await;
    let pending = scrobble_with(&f.state, &f.sending, item("old waiting"), |_, _| async { panic!("stale submission reached the transport") });
    tokio::pin!(pending);
    poll_fn(|cx| {
        assert!(pending.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    let new = f.replace(true, true);
    drop(blocked);
    tokio::time::timeout(WAIT, pending).await.unwrap();
    f.assert_replacement(&new);
}

#[tokio::test]
async fn current_session_responses_preserve_success_retry_and_revocation_behavior() {
    for code in [0, 9, 11] {
        let f = Fixture::new();
        scrobble_with(&f.state, &f.sending, item("current"), |batch, _| {
            assert_eq!(batch, vec![item("old backlog"), item("current")]);
            ready(response(code))
        })
        .await;
        let st = f.state.lock().unwrap();
        assert_eq!(st.session.is_none(), code == 9);
        if code == 11 {
            assert_eq!(st.queue, vec![item("old backlog"), item("current")]);
            let disk: Vec<Scrobble> = QueueFile(st.data_dir.join("lastfm_queue.json")).restore(Some(&st.session.as_ref().unwrap().name)).unwrap();
            assert_eq!(disk, st.queue);
        } else {
            assert!(st.queue.is_empty());
            assert!(!st.data_dir.join("lastfm_queue.json").exists());
        }
        assert_eq!(st.credentials.get(VAULT, &st.data_dir).is_none(), code == 9);
    }
}

#[tokio::test]
async fn a_backlog_is_sent_in_order_without_duplicates() {
    let f = Fixture::new();
    let mut expected: Vec<_> = (0..2 * BATCH + 1).map(|i| item(&format!("track {i}"))).collect();
    f.state.lock().unwrap().queue = expected.clone();
    expected.push(item("current"));
    let seen = Mutex::new(Vec::new());
    scrobble_with(&f.state, &f.sending, item("current"), |batch, _| {
        assert!(batch.len() <= BATCH);
        seen.lock().unwrap().extend(batch);
        ready(response(0))
    })
    .await;
    assert_eq!(*seen.lock().unwrap(), expected);
    assert!(f.state.lock().unwrap().queue.is_empty());
}

#[tokio::test]
async fn pending_requests_are_durable_and_serialized_before_either_answer() {
    let f = Fixture::new();
    let seen = Mutex::new(Vec::new());
    let (entered_tx, entered_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let mut once = Some((entered_tx, release_rx));
    let first = scrobble_with(&f.state, &f.sending, item("first"), |batch, _| {
        seen.lock().unwrap().extend(batch.clone());
        let wait = once.take();
        async move {
            if let Some((entered, release)) = wait {
                entered.send(batch).unwrap();
                release.await.unwrap();
            }
            response(0)
        }
    });
    tokio::pin!(first);
    poll_fn(|cx| {
        assert!(first.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    assert_eq!(entered_rx.await.unwrap(), vec![item("old backlog"), item("first")]);
    let second = scrobble_with(&f.state, &f.sending, item("second"), |batch, _| {
        seen.lock().unwrap().extend(batch);
        ready(response(0))
    });
    tokio::pin!(second);
    poll_fn(|cx| {
        assert!(second.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    let expected = vec![item("old backlog"), item("first"), item("second")];
    // Simulate a restart while the first HTTP request and second send lock are pending.
    let restored = load_state(&f.profile.path, f.credentials.clone());
    assert_eq!(restored.queue, expected);
    release_tx.send(()).unwrap();
    tokio::time::timeout(WAIT, async {
        tokio::join!(first, second);
    })
    .await
    .unwrap();
    assert_eq!(*seen.lock().unwrap(), expected);
    assert!(f.state.lock().unwrap().queue.is_empty());
}

#[test]
fn unavailable_credentials_preserve_a_legacy_queue_without_assigning_it_to_a_new_login() {
    for same_name in [false, true] {
        let f = Fixture::new();
        let path = f.profile.path.join("lastfm_queue.json");
        let bytes = serde_json::to_vec(&vec![item("legacy")]).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        f.credentials.unavailable.store(true, Ordering::Relaxed);
        let mut st = load_state(&f.profile.path, f.credentials.clone());
        assert!(st.session.is_none() && st.queue.is_empty());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let name = if same_name { "old user" } else { "new user" };
        keep_session(&mut st, Session { name: name.into(), key: "new secret".into() }).unwrap();
        assert!(st.queue.is_empty());
        assert!(!path.exists());
        assert_eq!(f.profile.preserved(), vec![bytes]);
        st.queue.push(item("new"));
        save_queue(&st).unwrap();
        assert_eq!(QueueFile(path).restore::<Scrobble>(Some(name)).unwrap(), vec![item("new")]);
    }
}

#[test]
fn startup_credentials_migrate_legacy_queues_and_recover_after_a_locked_vault() {
    let f = Fixture::new();
    let path = f.profile.path.join("lastfm_queue.json");
    std::fs::write(&path, serde_json::to_vec(&vec![item("legacy")]).unwrap()).unwrap();
    let migrated = load_state(&f.profile.path, f.credentials.clone());
    assert_eq!(migrated.queue, vec![item("legacy")]);
    let disk: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(disk["owner"], "old user");
    assert_eq!(disk["version"], 1);
    assert!(!String::from_utf8(std::fs::read(&path).unwrap()).unwrap().contains("old secret"));
    f.credentials.unavailable.store(true, Ordering::Relaxed);
    let locked = load_state(&f.profile.path, f.credentials.clone());
    assert!(locked.queue.is_empty());
    f.credentials.unavailable.store(false, Ordering::Relaxed);
    let restored = load_state(&f.profile.path, f.credentials.clone());
    assert_eq!(restored.queue, migrated.queue);
}

#[test]
fn a_known_owner_is_required_on_restore_and_same_account_rotation_preserves_the_backlog() {
    let f = Fixture::new();
    let path = f.profile.path.join("lastfm_queue.json");
    let before = std::fs::read(&path).unwrap();
    f.credentials.set(VAULT, &f.profile.path, b"other user\nother secret").unwrap();
    let mut st = load_state(&f.profile.path, f.credentials.clone());
    assert!(st.queue.is_empty());
    keep_session(&mut st, Session { name: "other user".into(), key: "rotated".into() }).unwrap();
    assert_eq!(f.profile.preserved(), vec![before]);
    assert!(st.queue.is_empty());
    st.queue.push(item("owned"));
    save_queue(&st).unwrap();
    let old = st.session.clone().unwrap();
    keep_session(&mut st, Session { name: "other user".into(), key: "rotated again".into() }).unwrap();
    assert_eq!(st.queue, vec![item("owned")]);
    assert!(!is_current(&st, &old));
}

#[test]
fn failed_queue_preservation_or_credential_write_keeps_the_previous_identity() {
    let f = Fixture::new();
    let mut st = f.state.lock().unwrap();
    let old = st.session.clone().unwrap();
    let credential = f.credentials.value();
    let path = f.profile.path.join("lastfm_queue.json");
    let saved = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(keep_session(&mut st, Session { name: "new user".into(), key: "new secret".into() }).is_err());
    assert!(is_current(&st, &old));
    assert_eq!(f.credentials.value(), credential);
    std::fs::remove_dir(&path).unwrap();
    std::fs::write(&path, saved).unwrap();
    f.credentials.reject_writes.store(true, Ordering::Relaxed);
    assert!(keep_session(&mut st, Session { name: "new user".into(), key: "new secret".into() }).is_err());
    assert!(is_current(&st, &old));
    assert_eq!(f.credentials.value(), credential);
    assert_eq!(st.queue, vec![item("old backlog")]);
    save_queue(&st).unwrap();
    assert_eq!(QueueFile(path).restore::<Scrobble>(Some("old user")).unwrap(), st.queue);
}

#[tokio::test]
async fn an_enqueue_write_failure_prevents_network_submission() {
    let f = Fixture::new();
    let path = f.profile.path.join("lastfm_queue.json");
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    scrobble_with(&f.state, &f.sending, item("unsaved"), |_, _| async { panic!("unpersisted play reached the transport") }).await;
    assert_eq!(f.state.lock().unwrap().queue, vec![item("old backlog"), item("unsaved")]);
}

#[tokio::test]
async fn now_playing_captures_the_session_before_spawn_and_ignores_stale_work() {
    for signed_in in [false, true] {
        let f = Fixture::new();
        if !signed_in {
            disconnect(&mut f.state.lock().unwrap());
        }
        let pending = now_playing_with(&f.state, |_| async { panic!("stale now playing reached the transport") });
        let new = f.replace(false, true);
        pending.await;
        f.assert_replacement(&new);
        now_playing_with(&f.state, |session| {
            assert!(Arc::ptr_eq(&session, &new));
            ready(Ok(()))
        })
        .await;
    }
}

#[tokio::test]
async fn a_failed_startup_read_keeps_recovered_backlog_safe_until_a_guarded_login() {
    let f = Fixture::new();
    let path = f.profile.path.join("lastfm_queue.json");
    let bytes = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let state = Mutex::new(load_state(&f.profile.path, f.credentials.clone()));
    assert!(state.lock().unwrap().session.is_none());
    std::fs::remove_dir(&path).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    scrobble_with(&state, &f.sending, item("unrestored"), |_, _| async { panic!("unrestored state reached transport") }).await;
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let mut st = state.lock().unwrap();
    keep_session(&mut st, Session { name: "old user".into(), key: "rotated".into() }).unwrap();
    assert_eq!(st.queue, vec![item("old backlog")]);
}

#[tokio::test]
async fn an_active_sender_never_sends_a_concurrently_appended_unpersisted_item() {
    let f = Fixture::new();
    let (send, entered, release) = delayed();
    let first = scrobble_with(&f.state, &f.sending, item("first"), send);
    tokio::pin!(first);
    poll_fn(|cx| {
        assert!(first.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    assert_eq!(entered.await.unwrap(), vec![item("old backlog"), item("first")]);
    let path = f.profile.path.join("lastfm_queue.json");
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    scrobble_with(&f.state, &f.sending, item("unsaved"), |_, _| async { panic!("unpersisted play reached transport") }).await;
    release.send(response(0)).unwrap();
    tokio::time::timeout(WAIT, first).await.unwrap();
    assert_eq!(f.state.lock().unwrap().queue, vec![item("unsaved")]);
}
