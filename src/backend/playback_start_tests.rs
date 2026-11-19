use std::future::pending;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use rodio::Source;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

use super::{COLD_SOLVER_HEAD_START, OpeningStream, STREAM_HEAD_START, first_ready};
use crate::audio::tempo::Tempo;
use crate::stream::{self, Shared};

const WAIT: Duration = Duration::from_secs(5);
const HEAD_START: Duration = Duration::from_millis(75);

#[tokio::test]
async fn signed_in_codecs_that_are_both_ready_use_the_earlier_pcm() {
    let early = Instant::now();
    let late = early + Duration::from_millis(10);
    for (aac, opus) in [(late, early), (early, late)] {
        let ready = super::earliest_pcm(std::future::ready(Ok(aac)), std::future::ready(Ok(opus)), |at| *at).await.unwrap();
        assert_eq!(ready, early);
    }
}

#[tokio::test]
async fn fast_primary_does_not_poll_alternate() {
    let result = first_ready(
        async {
            tokio::task::yield_now().await;
            Ok(1)
        },
        async { panic!("alternate was polled") },
        Duration::from_secs(60),
    );
    assert_eq!(timeout(WAIT, result).await.unwrap().unwrap(), 1);
}

#[tokio::test]
async fn primary_failure_starts_the_alternate_without_waiting_for_the_head_start() {
    let race = first_ready(async { anyhow::bail!("primary failed") }, async { Ok(2) }, Duration::from_secs(60));
    assert_eq!(timeout(WAIT, race).await.unwrap().unwrap(), 2);
}

#[tokio::test]
async fn primary_can_win_after_the_alternate_has_started() {
    let (finish_primary, primary_ready) = oneshot::channel();
    let (alternate_started, started) = oneshot::channel();
    let alternate_buffer = Shared::new(1, 0, None, false, false);
    let race = first_ready(
        async { Ok(primary_ready.await.unwrap()) },
        async {
            let _opening = OpeningStream(Some(alternate_buffer.clone()));
            alternate_started.send(()).unwrap();
            pending().await
        },
        HEAD_START,
    );
    let complete_primary = async {
        timeout(WAIT, started).await.unwrap().unwrap();
        assert!(alternate_buffer.filling());
        finish_primary.send(1).unwrap();
    };
    let (result, ()) = tokio::join!(timeout(WAIT, race), complete_primary);
    assert_eq!(result.unwrap().unwrap(), 1);
    assert!(!alternate_buffer.filling(), "losing buffer was not cancelled");
}

#[tokio::test]
async fn alternate_failure_does_not_end_a_pending_primary() {
    let (finish_primary, primary_ready) = oneshot::channel();
    let (alternate_failed, failed) = oneshot::channel();
    let mut race = Box::pin(first_ready(
        async { Ok(primary_ready.await.unwrap()) },
        async {
            alternate_failed.send(()).unwrap();
            anyhow::bail!("alternate failed");
        },
        HEAD_START,
    ));
    tokio::select! {
        biased;
        result = &mut race => panic!("race ended before primary finished: {result:?}"),
        result = timeout(WAIT, failed) => result.unwrap().unwrap(),
    };
    finish_primary.send(1).unwrap();
    assert_eq!(timeout(WAIT, race).await.unwrap().unwrap(), 1);
}

#[tokio::test]
async fn dropping_the_race_cancels_both_opening_buffers() {
    let primary = Shared::new(1, 0, None, false, false);
    let alternate = Shared::new(1, 0, None, false, false);
    let (both_started, started) = oneshot::channel();
    let mut race = Box::pin(first_ready(
        async {
            let _opening = OpeningStream(Some(primary.clone()));
            pending::<Result<()>>().await
        },
        async {
            let _opening = OpeningStream(Some(alternate.clone()));
            both_started.send(()).unwrap();
            pending::<Result<()>>().await
        },
        HEAD_START,
    ));
    tokio::select! {
        result = &mut race => panic!("pending race completed: {result:?}"),
        result = timeout(WAIT, started) => result.unwrap().unwrap(),
    };
    assert!(primary.filling() && alternate.filling());
    drop(race);
    assert!(!primary.filling() && !alternate.filling());
}

#[tokio::test]
async fn failure_from_both_candidates_returns_an_error() {
    let race = first_ready(async { anyhow::bail!("primary failed") }, async { anyhow::bail!("alternate failed") }, HEAD_START);
    let result: Result<()> = timeout(WAIT, race).await.unwrap();
    assert!(result.is_err());
}

const AAC: &[u8] = include_bytes!("../testdata/sine6-fragmented-aac.m4a");
const INVALID_MP4: &[u8] = &[0; 4096];
const SLOW_RESPONSE: Duration = Duration::from_millis(1800);
const FAST_RESPONSE: Duration = Duration::from_millis(25);

#[derive(Debug, PartialEq, Eq)]
enum FixtureSource {
    Primary,
    Alternate,
}

#[derive(Debug, PartialEq, Eq)]
enum Reply {
    Sent,
    PeerClosed,
}

struct FixtureServer {
    url: String,
    bytes: usize,
    task: JoinHandle<Result<Reply>>,
}

impl FixtureServer {
    async fn bind(body: &'static [u8], delay: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/audio?fixture=1", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await?;
            // The fixture fits a single stream block. Accept exactly its query range.
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut chunk = [0; 1024];
                let len = socket.read(&mut chunk).await?;
                anyhow::ensure!(len > 0 && request.len() < 8192, "invalid fixture request");
                request.extend_from_slice(&chunk[..len]);
            }
            let request_line = format!("GET /audio?fixture=1&range=0-{} HTTP/1.1\r\n", body.len() - 1);
            anyhow::ensure!(request.starts_with(request_line.as_bytes()), "unexpected fixture range");

            let mut unexpected = [0];
            tokio::select! {
                biased;
                result = socket.read(&mut unexpected) => {
                    anyhow::ensure!(matches!(result, Ok(0) | Err(_)), "unexpected fixture data");
                    Ok(Reply::PeerClosed)
                }
                () = sleep(delay) => {
                    let headers = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                    socket.write_all(headers.as_bytes()).await?;
                    socket.write_all(body).await?;
                    Ok(Reply::Sent)
                }
            }
        });
        Self { url, bytes: body.len(), task }
    }

    async fn outcome(&mut self) -> Reply {
        timeout(WAIT, &mut self.task).await.unwrap().unwrap().unwrap()
    }
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct PreparedPcm {
    source: FixtureSource,
    generation: u64,
    audio: Tempo<stream::TrackDecoder>,
}

async fn prepare_fixture(client: &reqwest::Client, server: &FixtureServer, source: FixtureSource, generation: u64) -> Result<PreparedPcm> {
    let shared = Shared::new(server.bytes as u64, 6000, None, false, false);
    let mut opening = OpeningStream(Some(shared.clone()));
    tokio::spawn(stream::download(client.clone(), stream::Media::File(server.url.clone()), "ytm-rs playback test".into(), shared.clone(), None));
    let audio = tokio::task::spawn_blocking(move || {
        let decoder = stream::decoder(shared, false)?;
        let audio = Tempo::new(decoder, Arc::default(), Duration::ZERO, Arc::default());
        anyhow::ensure!(audio.current_span_len().is_some_and(|len| len > 0), "no prepared PCM");
        Ok::<_, anyhow::Error>(audio)
    })
    .await??;
    opening.0.take();
    Ok(PreparedPcm { source, generation, audio })
}

async fn measure_race(client: &reqwest::Client, generation: u64, head_start: Duration) -> (PreparedPcm, Duration) {
    let mut primary = FixtureServer::bind(INVALID_MP4, SLOW_RESPONSE).await;
    let mut alternate = FixtureServer::bind(AAC, FAST_RESPONSE).await;
    let start = Instant::now();
    let pcm = timeout(
        WAIT,
        first_ready(
            prepare_fixture(client, &primary, FixtureSource::Primary, generation),
            prepare_fixture(client, &alternate, FixtureSource::Alternate, generation),
            head_start,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let elapsed = start.elapsed();
    assert_eq!(primary.outcome().await, Reply::PeerClosed);
    assert_eq!(alternate.outcome().await, Reply::Sent);
    (pcm, elapsed)
}

#[tokio::test]
async fn first_prepared_pcm_beats_the_sequential_fallback_on_loopback() {
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    // Model the former ordering: finish primary preparation, then try an alternate.
    let mut old_primary = FixtureServer::bind(INVALID_MP4, SLOW_RESPONSE).await;
    let mut old_alternate = FixtureServer::bind(AAC, FAST_RESPONSE).await;
    let start = Instant::now();
    let old = timeout(WAIT, async {
        assert!(prepare_fixture(&client, &old_primary, FixtureSource::Primary, 41).await.is_err());
        prepare_fixture(&client, &old_alternate, FixtureSource::Alternate, 41).await.unwrap()
    })
    .await
    .unwrap();
    let old_elapsed = start.elapsed();
    assert_eq!(old_primary.outcome().await, Reply::Sent);
    assert_eq!(old_alternate.outcome().await, Reply::Sent);

    let (new, new_elapsed) = measure_race(&client, 42, STREAM_HEAD_START).await;
    let (cold, cold_elapsed) = measure_race(&client, 44, COLD_SOLVER_HEAD_START).await;

    let mut normal = FixtureServer::bind(AAC, FAST_RESPONSE).await;
    let start = Instant::now();
    let control = timeout(WAIT, prepare_fixture(&client, &normal, FixtureSource::Primary, 43)).await.unwrap().unwrap();
    let control_elapsed = start.elapsed();
    assert_eq!(normal.outcome().await, Reply::Sent);

    // Timing ends at PCM preparation before reading the comparison samples.
    for (label, pcm, elapsed) in [
        ("sequential", &old, old_elapsed),
        ("warm hedge", &new, new_elapsed),
        ("cold hedge", &cold, cold_elapsed),
        ("control", &control, control_elapsed),
    ] {
        println!("{label}: {:?} generation {} -> PCM in {elapsed:?}", pcm.source, pcm.generation);
    }
    assert_eq!((old.source, old.generation), (FixtureSource::Alternate, 41));
    assert_eq!((new.source, new.generation), (FixtureSource::Alternate, 42));
    assert_eq!((control.source, control.generation), (FixtureSource::Primary, 43));
    assert_eq!((cold.source, cold.generation), (FixtureSource::Alternate, 44));
    let samples: Vec<_> = control.audio.take(8192).collect();
    assert_eq!(samples.len(), 8192);
    assert!(samples.iter().all(|sample| sample.is_finite()));
    assert!(samples.iter().any(|sample| sample.abs() > 0.001));
    assert!(old.audio.take(samples.len()).eq(samples.iter().copied()));
    assert!(cold.audio.take(samples.len()).eq(samples.iter().copied()));
    assert!(new.audio.take(samples.len()).eq(samples));
}
