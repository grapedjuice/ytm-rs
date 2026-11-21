use super::*;
use std::net::{SocketAddr, TcpListener as StdListener};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

const WAIT: Duration = Duration::from_secs(5);
const CREDS: &str = "user%40example:p%3Aa%2Bss@";
const AUTH: &str = "Proxy-Authorization: Basic dXNlckBleGFtcGxlOnA6YStzcw==\r\n";
type Server = JoinHandle<anyhow::Result<()>>;

fn trusted_native() -> native_tls::TlsConnector {
    native_tls::TlsConnector::builder()
        .disable_built_in_roots(true)
        .add_root_certificate(native_tls::Certificate::from_der(include_bytes!("testdata/ca.der")).unwrap())
        .build()
        .unwrap()
}

fn trusted() -> TlsConnector {
    trusted_native().into()
}

fn acceptor(expired: bool) -> tokio_native_tls::TlsAcceptor {
    let bytes: &[u8] = if expired { include_bytes!("testdata/expired.p12") } else { include_bytes!("testdata/proxy.p12") };
    let identity = native_tls::Identity::from_pkcs12(bytes, "test-only").unwrap();
    native_tls::TlsAcceptor::new(identity).unwrap().into()
}

async fn head<S: AsyncRead + Unpin>(socket: &mut S) -> anyhow::Result<String> {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        anyhow::ensure!(bytes.len() < 8192, "oversized test header");
        bytes.push(socket.read_u8().await?);
    }
    Ok(String::from_utf8(bytes)?)
}

async fn plain_server<F, R>(serve: F) -> (SocketAddr, Server)
where
    F: FnOnce(TcpStream) -> R + Send + 'static,
    R: Future<Output = anyhow::Result<()>> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (socket, _) = listener.accept().await?;
        serve(socket).await
    });
    (addr, task)
}

async fn tls_server<F, R>(expired: bool, serve: F) -> (SocketAddr, Server)
where
    F: FnOnce(tokio_native_tls::TlsStream<TcpStream>) -> R + Send + 'static,
    R: Future<Output = anyhow::Result<()>> + Send + 'static,
{
    let acceptor = acceptor(expired);
    plain_server(move |socket| async move { serve(acceptor.accept(socket).await?).await }).await
}

async fn finish(server: Server) -> anyhow::Result<()> {
    tokio::time::timeout(WAIT, server).await.unwrap().unwrap()
}

fn proxy_url(scheme: &str, addr: SocketAddr, auth: bool) -> url::Url {
    let host = if scheme == "https" { "localhost" } else { "127.0.0.1" };
    url::Url::parse(&format!("{scheme}://{}{host}:{}", if auth { CREDS } else { "" }, addr.port())).unwrap()
}

async fn echo_tunnel<S: AsyncRead + AsyncWrite + Unpin>(mut socket: S, auth: bool) -> anyhow::Result<()> {
    let request = head(&mut socket).await?;
    assert!(request.starts_with("CONNECT destination.test:443 HTTP/1.1\r\nHost: destination.test:443\r\n"));
    assert_eq!(request.contains(AUTH), auth);
    // Include tunnel data in the same write as the response header, to catch over-reading.
    socket.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\nready").await?;
    let mut ping = [0; 4];
    socket.read_exact(&mut ping).await?;
    assert_eq!(&ping, b"ping");
    socket.write_all(b"pong").await?;
    socket.shutdown().await?;
    Ok(())
}

async fn round_trip<S: AsyncRead + AsyncWrite + Unpin>(socket: &mut S) {
    tokio::time::timeout(WAIT, async {
        let mut prefix = [0; 5];
        socket.read_exact(&mut prefix).await.unwrap();
        assert_eq!(&prefix, b"ready");
        socket.write_all(b"ping").await.unwrap();
        let mut pong = [0; 4];
        socket.read_exact(&mut pong).await.unwrap();
        assert_eq!(&pong, b"pong");
        socket.shutdown().await.unwrap();
    })
    .await
    .unwrap();
}

fn fallback_target() -> StdListener {
    let listener = StdListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    listener
}

fn assert_no_fallback(listener: &StdListener) {
    assert_eq!(listener.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
}

#[tokio::test]
async fn https_connect_is_verified_authenticated_and_preserves_tunnel_bytes() {
    for auth in [false, true] {
        let (addr, server) = tls_server(false, move |socket| echo_tunnel(socket, auth)).await;
        let proxy = proxy_url("https", addr, auth);
        let mut socket = connect_via("destination.test", 443, Some(&proxy), Some(&trusted()), WAIT).await.unwrap();
        assert!(matches!(&socket, Connection::NativeTls(_)));
        round_trip(&mut socket).await;
        finish(server).await.unwrap();
    }
}

#[tokio::test]
async fn invalid_proxy_certificates_fail_before_connect_or_credentials_without_fallback() {
    // Unknown CA, valid CA with the wrong hostname, and an expired certificate.
    for case in 0..3 {
        let (captured_tx, captured_rx) = tokio::sync::oneshot::channel();
        let (addr, server) = tls_server(case == 2, move |mut socket| async move {
            let received = head(&mut socket).await.unwrap_or_default();
            let _ = captured_tx.send(received);
            Ok(())
        })
        .await;
        let mut proxy = proxy_url("https", addr, true);
        if case == 1 {
            proxy.set_host(Some("127.0.0.1")).unwrap();
        }
        let tls = trusted();
        let target = fallback_target();
        let result = connect_via("127.0.0.1", target.local_addr().unwrap().port(), Some(&proxy), if case == 0 { None } else { Some(&tls) }, WAIT).await;
        assert!(result.is_err(), "invalid certificate case {case} was accepted");
        let _ = finish(server).await;
        assert!(captured_rx.await.unwrap_or_default().is_empty(), "CONNECT or credentials sent before verification");
        assert_no_fallback(&target);
    }
}

#[tokio::test]
async fn http_connect_and_authentication_still_work() {
    for auth in [false, true] {
        let (addr, server) = plain_server(move |socket| echo_tunnel(socket, auth)).await;
        let proxy = proxy_url("http", addr, auth);
        let mut socket = connect_via("destination.test", 443, Some(&proxy), None, WAIT).await.unwrap();
        assert!(matches!(&socket, Connection::Plain(_)));
        round_trip(&mut socket).await;
        finish(server).await.unwrap();
    }
}

#[tokio::test]
async fn http_and_https_rejections_never_connect_directly() {
    async fn refuse<S: AsyncRead + AsyncWrite + Unpin>(mut socket: S) -> anyhow::Result<()> {
        let request = head(&mut socket).await?;
        assert!(request.contains(AUTH));
        socket.write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\n\r\n").await?;
        Ok(())
    }
    for secure in [false, true] {
        let (addr, server) = if secure { tls_server(false, refuse).await } else { plain_server(refuse).await };
        let proxy = proxy_url(if secure { "https" } else { "http" }, addr, true);
        let target = fallback_target();
        let result = connect_via("127.0.0.1", target.local_addr().unwrap().port(), Some(&proxy), Some(&trusted()), WAIT).await;
        assert!(result.unwrap_err().to_string().contains("407"));
        finish(server).await.unwrap();
        assert_no_fallback(&target);
    }
}

#[tokio::test]
async fn direct_connections_remain_plain_tcp() {
    let (addr, server) = plain_server(|mut socket| async move {
        socket.write_all(b"ready").await?;
        let mut ping = [0; 4];
        socket.read_exact(&mut ping).await?;
        assert_eq!(&ping, b"ping");
        socket.write_all(b"pong").await?;
        Ok(())
    })
    .await;
    let mut socket = connect_via("127.0.0.1", addr.port(), None, None, WAIT).await.unwrap();
    assert!(matches!(&socket, Connection::Plain(_)));
    round_trip(&mut socket).await;
    finish(server).await.unwrap();
}

async fn socks_server(mut socket: TcpStream, auth: bool, reject: bool) -> anyhow::Result<()> {
    let mut greeting = [0; 2];
    socket.read_exact(&mut greeting).await?;
    let mut methods = vec![0; greeting[1] as usize];
    socket.read_exact(&mut methods).await?;
    assert_eq!(greeting[0], 5);
    assert_eq!(methods, if auth { vec![0, 2] } else { vec![0] });
    socket.write_all(&[5, if auth { 2 } else { 0 }]).await?;
    if auth {
        assert_eq!(socket.read_u8().await?, 1);
        let mut user = vec![0; socket.read_u8().await? as usize];
        socket.read_exact(&mut user).await?;
        let mut pass = vec![0; socket.read_u8().await? as usize];
        socket.read_exact(&mut pass).await?;
        assert_eq!(user, b"user@example");
        assert_eq!(pass, b"p:a+ss");
        socket.write_all(&[1, u8::from(reject)]).await?;
        if reject {
            return Ok(());
        }
    }
    let mut request = [0; 5];
    socket.read_exact(&mut request).await?;
    assert_eq!(&request[..4], &[5, 1, 0, 3]);
    let mut host = vec![0; request[4] as usize];
    socket.read_exact(&mut host).await?;
    assert_eq!(host, b"destination.test");
    assert_eq!(socket.read_u16().await?, 443);
    socket.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0]).await?;
    socket.write_all(b"ready").await?;
    let mut ping = [0; 4];
    socket.read_exact(&mut ping).await?;
    assert_eq!(&ping, b"ping");
    socket.write_all(b"pong").await?;
    Ok(())
}

#[tokio::test]
async fn socks5_and_socks5h_keep_their_authentication_and_tunnels() {
    for scheme in ["socks5", "socks5h"] {
        for auth in [false, true] {
            let (addr, server) = plain_server(move |socket| socks_server(socket, auth, false)).await;
            let proxy = proxy_url(scheme, addr, auth);
            let mut socket = connect_via("destination.test", 443, Some(&proxy), None, WAIT).await.unwrap();
            round_trip(&mut socket).await;
            finish(server).await.unwrap();
        }
    }
}

#[tokio::test]
async fn rejected_socks_credentials_do_not_fall_back_to_direct() {
    let (addr, server) = plain_server(|socket| socks_server(socket, true, true)).await;
    let proxy = proxy_url("socks5", addr, true);
    let target = fallback_target();
    let result = connect_via("127.0.0.1", target.local_addr().unwrap().port(), Some(&proxy), None, WAIT).await;
    assert!(result.unwrap_err().to_string().contains("rejected"));
    finish(server).await.unwrap();
    assert_no_fallback(&target);
}

#[tokio::test]
async fn ipv6_connect_targets_are_bracketed_without_consuming_tunnel_data() {
    let (mut client, mut server) = tokio::io::duplex(1024);
    let respond = async {
        assert_eq!(head(&mut server).await.unwrap(), "CONNECT [::1]:443 HTTP/1.1\r\nHost: [::1]:443\r\n\r\n");
        server.write_all(b"HTTP/1.1 200 OK\r\n\r\nx").await.unwrap();
    };
    let proxy = parse("http://localhost:8080").unwrap();
    let (result, _) = tokio::join!(http_connect(&mut client, &proxy, "[::1]", 443), respond);
    result.unwrap();
    assert_eq!(client.read_u8().await.unwrap(), b'x');
    assert_eq!(bare_host("[::1]"), "::1");
}

#[tokio::test]
async fn stalled_proxy_tls_and_connect_handshakes_have_an_establishment_deadline() {
    for secure in [false, true] {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (addr, server) = plain_server(move |mut socket| async move {
            if !secure {
                head(&mut socket).await?;
            }
            let _ = entered_tx.send(());
            std::future::pending::<()>().await;
            Ok(())
        })
        .await;
        let mut proxy = proxy_url(if secure { "https" } else { "http" }, addr, false);
        // No name resolution / IPv6 fallback inside this short deadline: the
        // fixture specifically tests a stalled TLS or CONNECT exchange.
        proxy.set_host(Some("127.0.0.1")).unwrap();
        let result = connect_via("destination.test", 443, Some(&proxy), Some(&trusted()), Duration::from_millis(500)).await;
        assert!(result.unwrap_err().to_string().contains("timed out"));
        tokio::time::timeout(WAIT, entered_rx).await.unwrap().unwrap();
        server.abort();
    }
}

async fn relay(proxy: url::Url, tls: Option<TlsConnector>) -> (SocketAddr, Server) {
    plain_server(move |mut client| async move { relay_with(&mut client, |host, port| async move { connect_via(&host, port, Some(&proxy), tls.as_ref(), WAIT).await }).await }).await
}

#[tokio::test]
async fn sign_in_relay_carries_a_verified_https_tunnel() {
    let (addr, proxy_server) = tls_server(false, |socket| echo_tunnel(socket, true)).await;
    let (relay_addr, relay_server) = relay(proxy_url("https", addr, true), Some(trusted())).await;
    let mut socket = TcpStream::connect(relay_addr).await.unwrap();
    socket.write_all(b"CONNECT destination.test:443 HTTP/1.1\r\nHost: destination.test:443\r\n\r\n").await.unwrap();
    assert!(head(&mut socket).await.unwrap().starts_with("HTTP/1.1 200"));
    round_trip(&mut socket).await;
    finish(proxy_server).await.unwrap();
    finish(relay_server).await.unwrap();
}

#[tokio::test]
async fn sign_in_relay_returns_502_when_the_proxy_certificate_is_untrusted() {
    let (addr, proxy_server) = tls_server(false, |mut socket| async move {
        assert!(head(&mut socket).await.unwrap_or_default().is_empty(), "untrusted proxy received CONNECT");
        Ok(())
    })
    .await;
    let (relay_addr, relay_server) = relay(proxy_url("https", addr, true), None).await;
    let mut socket = TcpStream::connect(relay_addr).await.unwrap();
    socket.write_all(b"CONNECT destination.test:443 HTTP/1.1\r\n\r\n").await.unwrap();
    assert!(head(&mut socket).await.unwrap().starts_with("HTTP/1.1 502"));
    assert!(finish(relay_server).await.is_err());
    let _ = finish(proxy_server).await;
}

#[tokio::test]
async fn wss_adds_separate_target_tls_over_the_https_proxy_tunnel() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::{Connector, accept_async, client_async_tls_with_config, tungstenite::Message};
    for trust_target in [false, true] {
        let (target, target_server) = tls_server(false, |socket| async {
            let mut ws = accept_async(socket).await?;
            let message = ws.next().await.unwrap()?;
            assert_eq!(message, Message::text("nested TLS"));
            ws.send(message).await?;
            Ok(())
        })
        .await;
        let (addr, proxy_server) = tls_server(false, move |mut socket| async move {
            assert!(head(&mut socket).await?.starts_with(&format!("CONNECT localhost:{} HTTP/1.1\r\n", target.port())));
            let mut upstream = TcpStream::connect(target).await?;
            socket.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await?;
            tokio::io::copy_bidirectional(&mut socket, &mut upstream).await?;
            Ok(())
        })
        .await;
        let proxy = proxy_url("https", addr, false);
        let tunnel = connect_via("localhost", target.port(), Some(&proxy), Some(&trusted()), WAIT).await.unwrap();
        let target_tls = if trust_target { trusted_native() } else { native_tls::TlsConnector::new().unwrap() };
        let result = tokio::time::timeout(WAIT, client_async_tls_with_config(format!("wss://localhost:{}/", target.port()), tunnel, None, Some(Connector::NativeTls(target_tls))))
            .await
            .unwrap();
        if trust_target {
            let (mut ws, _) = result.unwrap();
            ws.send(Message::text("nested TLS")).await.unwrap();
            assert_eq!(tokio::time::timeout(WAIT, ws.next()).await.unwrap().unwrap().unwrap(), Message::text("nested TLS"));
            drop(ws);
            finish(target_server).await.unwrap();
            let _ = finish(proxy_server).await;
        } else {
            assert!(matches!(result, Err(tokio_tungstenite::tungstenite::Error::Tls(_))));
            let _ = finish(target_server).await;
            let _ = finish(proxy_server).await;
        }
    }
}
