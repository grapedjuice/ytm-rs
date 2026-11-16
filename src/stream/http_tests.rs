use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const TEST_WAIT: Duration = Duration::from_secs(3);

enum Reply {
    Bytes(Vec<u8>),
    Stall,
    Slow,
}

/// A finite HTTP fault script. Dropping it aborts even an accepted stalled socket.
struct Server {
    url: String,
    requests: mpsc::UnboundedReceiver<String>,
    task: Option<JoinHandle<()>>,
}

impl Server {
    async fn new(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/audio?test=1", listener.local_addr().unwrap());
        let (sent, requests) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            for reply in replies {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                loop {
                    let mut chunk = [0; 1024];
                    let n = socket.read(&mut chunk).await.unwrap();
                    assert!(n > 0, "client closed before sending its headers");
                    request.extend_from_slice(&chunk[..n]);
                    assert!(request.len() < 16 * 1024);
                    if request.windows(4).any(|w| w == b"\r\n\r\n") { break; }
                }
                match reply {
                    Reply::Bytes(bytes) => {
                        socket.write_all(&bytes).await.unwrap();
                        let _ = sent.send(String::from_utf8(request).unwrap());
                    }
                    Reply::Stall => {
                        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\n").await.unwrap();
                        let _ = sent.send(String::from_utf8(request).unwrap());
                        std::future::pending::<()>().await;
                    }
                    Reply::Slow => {
                        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 14\r\nConnection: close\r\n\r\na").await.unwrap();
                        let _ = sent.send(String::from_utf8(request).unwrap());
                        for _ in 0..13 {
                            tokio::time::sleep(Duration::from_secs(1)).await;
                            socket.write_all(b"a").await.unwrap();
                        }
                    }
                }
            }
        });
        Self { url, requests, task: Some(task) }
    }

    async fn request(&mut self) -> String {
        tokio::time::timeout(TEST_WAIT, self.requests.recv()).await.unwrap().expect("missing request")
    }

    async fn finish(&mut self) {
        tokio::time::timeout(TEST_WAIT, self.task.as_mut().unwrap()).await.unwrap().unwrap();
        self.task.take();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(task) = &self.task { task.abort(); }
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

fn response(status: &str, headers: &str, body: &str) -> Reply {
    Reply::Bytes(format!("HTTP/1.1 {status}\r\n{headers}Connection: close\r\n\r\n{body}").into_bytes())
}

struct Download(Option<JoinHandle<()>>);

impl Drop for Download {
    fn drop(&mut self) {
        if let Some(task) = &self.0 { task.abort(); }
    }
}

fn run(future: impl Future<Output = ()> + Send + 'static) -> Download {
    Download(Some(tokio::spawn(future)))
}

async fn finish_download(mut task: Download) {
    tokio::time::timeout(TEST_WAIT, task.0.as_mut().unwrap()).await.unwrap().unwrap();
    task.0.take();
}

#[tokio::test]
async fn file_partial_body_resumes_after_the_received_prefix() {
    let mut server = Server::new(vec![
        response("200 OK", "Content-Length: 6\r\n", "ABC"),
        response("200 OK", "Content-Length: 3\r\n", "DEF"),
    ]).await;
    let shared = Shared::new(6, 0, None, false, false);
    let task = run(download(client(), Media::File(server.url.clone()), "test".into(), shared.clone(), None));
    assert!(server.request().await.starts_with("GET /audio?test=1&range=0-5 "));
    assert!(server.request().await.starts_with("GET /audio?test=1&range=3-5 "));
    finish_download(task).await;
    server.finish().await;
    assert_eq!(shared.bytes().unwrap(), b"ABCDEF");
    assert!(shared.error().is_none());
}

#[tokio::test]
async fn feed_partial_body_resumes_with_the_suffix_range_header() {
    let mut server = Server::new(vec![
        response("206 Partial Content", "Content-Length: 6\r\nContent-Range: bytes 0-5/6\r\n", "ABC"),
        response("206 Partial Content", "Content-Length: 3\r\nContent-Range: bytes 3-5/6\r\n", "DEF"),
    ]).await;
    let shared = Shared::feed(6, 0, false);
    let task = run(download(client(), Media::Feed(server.url.clone()), "test".into(), shared.clone(), None));
    let first = server.request().await.to_ascii_lowercase();
    let second = server.request().await.to_ascii_lowercase();
    assert!(first.contains("\r\nrange: bytes=0-5\r\n"));
    assert!(second.contains("\r\nrange: bytes=3-5\r\n"));
    assert!(!first.contains("&range="));
    assert!(!second.contains("&range="));
    finish_download(task).await;
    server.finish().await;
    assert_eq!(shared.bytes().unwrap(), b"ABCDEF");
    assert!(shared.error().is_none());
}

#[tokio::test]
async fn hls_partial_body_restarts_the_whole_segment() {
    let mut server = Server::new(vec![
        response("200 OK", "Content-Length: 6\r\n", "ABC"),
        response("200 OK", "Content-Length: 6\r\n", "ABCDEF"),
    ]).await;
    let shared = Shared::segmented(vec![0.0], 1000, None, false);
    let task = run(download(client(), Media::Segments(vec![server.url.clone()]), "test".into(), shared.clone(), None));
    let first = server.request().await;
    let second = server.request().await;
    assert_eq!(first.lines().next(), second.lines().next());
    assert!(!second.to_ascii_lowercase().contains("range:"));
    assert!(!second.contains("&range="));
    finish_download(task).await;
    server.finish().await;
    assert_eq!(shared.bytes().unwrap(), b"ABCDEF");
}

#[tokio::test]
async fn feed_rejects_unverified_or_inconsistent_content_ranges() {
    for (status, header) in [
        ("200 OK", "Content-Range: bytes 0-5/6\r\n"),
        ("206 Partial Content", ""),
        ("206 Partial Content", "Content-Range: bytes 1-6/7\r\n"),
        ("206 Partial Content", "Content-Range: bytes 0-5/7\r\n"),
        ("206 Partial Content", "Content-Range: bytes 0-5/5\r\n"),
        ("206 Partial Content", "Content-Range: bytes 0-5/nonsense\r\n"),
    ] {
        let mut server = Server::new(vec![response(status, &format!("Content-Length: 6\r\n{header}"), "ABCDEF")]).await;
        let mut bytes = bytes::BytesMut::new();
        let result = tokio::time::timeout(TEST_WAIT,
            fetch(&client(), &server.url, "test", Range::Header(0, 5), 6, &mut bytes)).await.unwrap();
        assert!(result.is_err(), "accepted {status}, {header:?}");
        assert!(bytes.is_empty(), "unverified response was retained");
        server.finish().await;
    }
}

#[tokio::test]
async fn file_rejects_an_explicitly_wrong_content_range() {
    let mut server = Server::new(vec![response("206 Partial Content",
        "Content-Length: 6\r\nContent-Range: bytes 1-6/7\r\n", "ABCDEF")]).await;
    let mut bytes = bytes::BytesMut::new();
    assert!(tokio::time::timeout(TEST_WAIT,
        fetch(&client(), &server.url, "test", Range::Param(0, 5), 6, &mut bytes)).await.unwrap().is_err());
    assert!(bytes.is_empty());
    server.finish().await;
}

#[tokio::test]
async fn invalid_or_oversized_responses_preserve_only_the_previously_validated_prefix() {
    for reply in [
        response("200 OK", "Content-Length: 5\r\n", "CDEFG"),
        response("200 OK", "Transfer-Encoding: chunked\r\n", "2\r\nCD\r\n3\r\nEFG\r\n0\r\n\r\n"),
        response("200 OK", "Transfer-Encoding: chunked\r\n", "4\r\nCDEF\r\n"),
    ] {
        let mut server = Server::new(vec![reply]).await;
        let mut bytes = bytes::BytesMut::from(&b"AB"[..]);
        assert!(tokio::time::timeout(TEST_WAIT,
            fetch(&client(), &server.url, "test", Range::Param(2, 5), 6, &mut bytes)).await.unwrap().is_err());
        assert_eq!(&bytes[..], b"AB");
        server.finish().await;
    }
}

#[tokio::test]
async fn cancellation_interrupts_a_stalled_http_body() {
    let mut server = Server::new(vec![Reply::Stall]).await;
    let shared = Shared::new(6, 0, None, false, false);
    let task = run(download(client(), Media::File(server.url.clone()), "test".into(), shared.clone(), None));
    server.request().await;
    shared.cancel();
    finish_download(task).await;
    tokio::time::timeout(TEST_WAIT, shared.finished()).await.unwrap();
    assert!(shared.error().is_none());
    assert!(!shared.is_complete());
}

#[tokio::test]
async fn cancellation_interrupts_a_hanging_url_refresh() {
    let mut server = Server::new(vec![response("403 Forbidden", "Content-Length: 0\r\n", "")]).await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let notify = entered.clone();
    let refresh: Refresh = Arc::new(move || {
        notify.notify_one();
        Box::pin(std::future::pending())
    });
    let shared = Shared::new(6, 0, None, false, false);
    let task = run(download(client(), Media::File(server.url.clone()), "test".into(), shared.clone(), Some(refresh)));
    tokio::time::timeout(TEST_WAIT, entered.notified()).await.unwrap();
    shared.cancel();
    finish_download(task).await;
    server.finish().await;
    assert!(shared.error().is_none());
}

#[tokio::test]
async fn cancellation_stops_a_retrying_download() {
    let mut server = Server::new((0..3).map(|_| response("503 Service Unavailable", "Content-Length: 0\r\n", "")).collect()).await;
    let shared = Shared::new(6, 0, None, false, false);
    let task = run(download(client(), Media::File(server.url.clone()), "test".into(), shared.clone(), None));
    for _ in 0..3 { server.request().await; }
    // The third failure would start a 3.6 s backoff. Cancellation races response
    // handling, so this checks the retry sequence rather than an exact sleep state.
    shared.cancel();
    finish_download(task).await;
    assert!(shared.error().is_none());
}

#[tokio::test]
async fn cancellation_interrupts_waiting_for_the_playing_buffer() {
    let shared = Shared::new(6, 0, None, false, true);
    let playing = Shared::new(6, 0, None, false, false);
    let entered = Arc::new(tokio::sync::Notify::new());
    let notify = entered.clone();
    let ahead: Ahead = Arc::new(move || {
        notify.notify_one();
        Some(playing.clone())
    });
    let task = run(download_behind(client(), Media::File("http://127.0.0.1:9/unused".into()),
        "test".into(), shared.clone(), None, Some(ahead)));
    tokio::time::timeout(TEST_WAIT, entered.notified()).await.unwrap();
    shared.cancel();
    finish_download(task).await;
    assert!(shared.error().is_none());
}

#[tokio::test]
async fn terminal_head_prefetch_failure_wakes_idle_workers_and_finished() {
    let server = Server::new(vec![
        response("403 Forbidden", "Content-Length: 0\r\n", ""),
        response("403 Forbidden", "Content-Length: 0\r\n", ""),
    ]).await;
    let shared = Shared::new(BLOCK * 3, 0, None, false, true);
    let task = run(download(client(), Media::File(server.url.clone()), "test".into(), shared.clone(), None));
    finish_download(task).await;
    tokio::time::timeout(TEST_WAIT, shared.finished()).await.unwrap();
    assert!(shared.error().is_some());
}

#[test]
fn complete_cache_can_be_replayed_after_reader_drop_and_global_cancel() {
    let shared = Shared::from_bytes(b"ABCDEF".to_vec(), 0, None);
    let mut first = StreamReader::new(shared.clone());
    let mut bytes = Vec::new();
    first.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"ABCDEF");
    drop(first);
    shared.cancel();
    let mut second = StreamReader::new(shared.clone());
    bytes.clear();
    second.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"ABCDEF");
    assert!(shared.is_complete());
}

#[tokio::test]
async fn a_continuously_progressing_download_may_take_longer_than_thirteen_seconds() {
    let mut server = Server::new(vec![Reply::Slow]).await;
    let shared = Shared::new(14, 0, None, false, false);
    let started = std::time::Instant::now();
    tokio::time::timeout(Duration::from_secs(18),
        download(client(), Media::File(server.url.clone()), "test".into(), shared.clone(), None)).await.unwrap();
    server.finish().await;
    assert!(started.elapsed() >= Duration::from_secs(13));
    assert_eq!(shared.bytes().unwrap(), vec![b'a'; 14]);
    assert!(shared.error().is_none());
}


#[tokio::test]
async fn feed_probe_requires_the_requested_byte_and_a_known_total() {
    for (range, valid) in [
        ("bytes 0-0/6", true),
        ("bytes 1-1/6", false),
        ("bytes 0-1/6", false),
        ("bytes 0-0/*", false),
        ("bytes 0-0/0", false),
    ] {
        let mut server = Server::new(vec![response("206 Partial Content",
            &format!("Content-Length: 1\r\nContent-Range: {range}\r\n"), "A")]).await;
        let result = tokio::time::timeout(TEST_WAIT, probe_feed(&client(), &server.url, "test")).await.unwrap();
        if valid {
            let (url, total) = result.unwrap();
            assert_eq!(url, server.url);
            assert_eq!(total, 6);
        } else {
            assert!(result.is_err(), "accepted {range}");
        }
        server.finish().await;
    }
}

#[test]
fn an_incomplete_cancelled_buffer_keeps_its_prefix_without_reviving_missing_reads() {
    let shared = Shared::new(FIRST + 1, 0, None, false, false);
    shared.put(0, Bytes::from(vec![b'A'; FIRST as usize]));
    drop(StreamReader::new(shared.clone()));
    let mut reader = StreamReader::new(shared.clone());
    let mut prefix = vec![0; FIRST as usize];
    reader.read_exact(&mut prefix).unwrap();
    assert_eq!(prefix, vec![b'A'; FIRST as usize]);
    let (sent, received) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || { let _ = sent.send(reader.read(&mut [0])); });
    let result = received.recv_timeout(TEST_WAIT);
    // Ensure a regressed implementation cannot leave a blocked test thread behind.
    shared.cancel();
    worker.join().unwrap();
    assert!(result.expect("a new reader revived a cancelled download").is_err());
    assert!(!shared.is_complete());
}

