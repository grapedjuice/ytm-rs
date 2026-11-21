use super::*;
use std::future::{poll_fn, ready};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
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
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ytm-lastfm-race-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        let fixture = Self { state: Mutex::new(State { session: None, data_dir: dir, queue: vec![item("old backlog")] }), sending: tokio::sync::Mutex::new(()) };
        {
            let mut st = fixture.state.lock().unwrap();
            keep_session(&mut st, Session { name: "old user".into(), key: "old secret".into() }).unwrap();
            save_queue(&st);
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
        assert!(st.queue.is_empty(), "the replaced session's backlog must not move to the new account");
        st.queue.push(item("new backlog"));
        save_queue(&st);
        st.session.clone().unwrap()
    }

    fn assert_replacement(&self, session: &Arc<Session>) {
        let st = self.state.lock().unwrap();
        assert!(is_current(&st, session));
        assert_eq!(st.queue, vec![item("new backlog")]);
        let disk: Vec<Scrobble> = serde_json::from_slice(&std::fs::read(st.data_dir.join("lastfm_queue.json")).unwrap()).unwrap();
        assert_eq!(disk, st.queue);
        assert_eq!(crate::vault::get(VAULT, &st.data_dir), Some(format!("{}\n{}", session.name, session.key).into_bytes()));
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Every credential is scoped to an exclusively created test directory.
        // No real account, global service state or default-profile vault entry is used.
        let st = self.state.get_mut().unwrap_or_else(|e| e.into_inner());
        crate::vault::delete(VAULT, &st.data_dir);
        let _ = std::fs::remove_file(st.data_dir.join("lastfm_queue.json"));
        let _ = std::fs::remove_dir(&st.data_dir);
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
        assert!(crate::vault::get(VAULT, &st.data_dir).is_none());
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
                scrobble_with(&f.state, &f.sending, item("new listen"), |batch, session| {
                    assert!(Arc::ptr_eq(&session, &new));
                    assert_eq!(batch, vec![item("new backlog"), item("new listen")]);
                    ready(response(0))
                })
                .await;
                let st = f.state.lock().unwrap();
                assert!(is_current(&st, &new) && st.queue.is_empty());
                assert!(!st.data_dir.join("lastfm_queue.json").exists());
                assert_eq!(crate::vault::get(VAULT, &st.data_dir), Some(format!("{}\n{}", new.name, new.key).into_bytes()));
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
            let disk: Vec<Scrobble> = serde_json::from_slice(&std::fs::read(st.data_dir.join("lastfm_queue.json")).unwrap()).unwrap();
            assert_eq!(disk, st.queue);
        } else {
            assert!(st.queue.is_empty());
            assert!(!st.data_dir.join("lastfm_queue.json").exists());
        }
        assert_eq!(crate::vault::get(VAULT, &st.data_dir).is_none(), code == 9);
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
async fn concurrent_submissions_for_one_session_are_serialized() {
    let f = Fixture::new();
    let (send, entered, release) = delayed();
    let first = scrobble_with(&f.state, &f.sending, item("first"), send);
    let second = scrobble_with(&f.state, &f.sending, item("second"), |batch, _| {
        assert_eq!(batch, vec![item("second")]);
        ready(response(0))
    });
    let unblock = async {
        assert_eq!(entered.await.unwrap(), vec![item("old backlog"), item("first")]);
        release.send(response(0)).unwrap();
    };
    tokio::time::timeout(WAIT, async {
        tokio::join!(first, second, unblock);
    })
    .await
    .unwrap();
    assert!(f.state.lock().unwrap().queue.is_empty());
}
