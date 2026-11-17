//! Smoke test for the Jam relay. Run `npx wrangler dev` in server/ first, then
//! `cargo run` here. Override the target with JAM_URL (default ws://127.0.0.1:8787).

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;
type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";

fn base() -> String {
    std::env::var("JAM_URL").unwrap_or_else(|_| "ws://127.0.0.1:8787".into())
}

fn new_code() -> String {
    use std::hash::{BuildHasher, RandomState};
    let mut n = RandomState::new().hash_one(std::time::SystemTime::now());
    (0..6)
        .map(|_| {
            let c = ALPHABET[(n % ALPHABET.len() as u64) as usize] as char;
            n /= ALPHABET.len() as u64;
            c
        })
        .collect()
}

async fn connect(code: &str, create: bool, name: &str) -> R<Ws> {
    let url = format!("{}/jam/{code}?create={}&name={name}&v=1", base(), create as u8);
    Ok(connect_async(url).await?.0)
}

async fn send(ws: &mut Ws, v: Value) -> R {
    ws.send(Message::text(v.to_string())).await?;
    Ok(())
}

/// Next frame, or None on timeout / close.
async fn next_frame(ws: &mut Ws, ms: u64) -> Option<Message> {
    match timeout(Duration::from_millis(ms), ws.next()).await {
        Ok(Some(Ok(m))) => Some(m),
        _ => None,
    }
}

/// Waits for the next JSON message of type `t`, skipping others.
async fn expect(ws: &mut Ws, t: &str) -> R<Value> {
    loop {
        match next_frame(ws, 3000).await {
            Some(Message::Text(s)) => {
                let v: Value = serde_json::from_str(&s)?;
                if v["t"] == t {
                    return Ok(v);
                }
            }
            Some(Message::Close(f)) => return Err(format!("closed ({f:?}) waiting for {t}").into()),
            Some(_) => {}
            None => return Err(format!("timed out waiting for {t}").into()),
        }
    }
}

/// Asserts no JSON message of type `t` arrives within a short window.
async fn expect_none(ws: &mut Ws, t: &str) -> R {
    while let Some(m) = next_frame(ws, 400).await {
        if let Message::Text(s) = m {
            let v: Value = serde_json::from_str(&s)?;
            if v["t"] == t {
                return Err(format!("unexpected {t}: {v}").into());
            }
        }
    }
    Ok(())
}

async fn expect_close(ws: &mut Ws, code: u16) -> R {
    loop {
        match next_frame(ws, 3000).await {
            Some(Message::Close(Some(f))) if u16::from(f.code) == code => return Ok(()),
            Some(Message::Close(f)) => return Err(format!("wrong close: {f:?}").into()),
            Some(_) => {}
            None => return Err(format!("no close {code}").into()),
        }
    }
}

async fn expect_error(ws: &mut Ws, code: &str) -> R {
    let e = expect(ws, "error").await?;
    check(e["code"] == code, &format!("error code {code}, got {e}"))
}

fn check(cond: bool, what: &str) -> R {
    if cond { Ok(()) } else { Err(format!("check failed: {what}").into()) }
}

async fn http_get(path: &str) -> R<String> {
    let host = base().trim_start_matches("ws://").to_string();
    let mut s = TcpStream::connect(&host).await?;
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes()).await?;
    let mut out = String::new();
    s.read_to_string(&mut out).await?;
    Ok(out)
}

async fn stats() -> R<Value> {
    let r = http_get("/stats").await?;
    check(r.starts_with("HTTP/1.1 200"), &r)?;
    let (start, end) = (r.find('{').ok_or("no json")?, r.rfind('}').ok_or("no json")?);
    Ok(serde_json::from_str(&r[start..=end])?)
}

macro_rules! step {
    ($name:expr, $body:expr) => {{
        match $body {
            Ok(v) => { println!("ok   {}", $name); v }
            Err(e) => { println!("FAIL {}: {e}", $name); std::process::exit(1) }
        }
    }};
}

#[tokio::main]
async fn main() -> R {
    let code = new_code();
    println!("room {code} on {}", base());

    // The raw HTTP check speaks plain TCP: local `wrangler dev` only.
    if base().starts_with("ws://") {
        step!("GET / -> 200", async {
            let r = http_get("/").await?;
            check(r.starts_with("HTTP/1.1 200") && r.ends_with("ytm-rs jam server"), &r)
        }.await);
    } else {
        println!("skip GET / -> 200 (TLS)");
    }

    step!("invalid code -> 400", async {
        match connect_async(format!("{}/jam/ABC0OI?create=0&v=1", base())).await {
            Err(tungstenite::Error::Http(resp)) if resp.status() == 400 => R::Ok(()),
            Err(e) => Err(format!("{e}").into()),
            Ok(_) => Err("upgrade succeeded".into()),
        }
    }.await);

    step!("join unknown room -> not_found", async {
        let mut ws = connect(&new_code(), false, "x").await?;
        expect_error(&mut ws, "not_found").await?;
        expect_close(&mut ws, 4004).await
    }.await);

    let mut a = step!("create", connect(&code, true, "%20%20Al%07ice%20%20").await);
    let wa = step!("welcome contents", async {
        let w = expect(&mut a, "welcome").await?;
        check(w["you"].is_string() && w["you"] == w["host"], "creator is host")?;
        check(w["control"] == false, "control starts off")?;
        check(w["queue"] == true, "guests may add songs by default")?;
        check(w["members"] == json!([{ "id": w["you"], "name": "Alice" }]), &format!("members/name cleaned: {w}"))?;
        check(w["state"].is_null() && w["tick"].is_null(), "no state yet")?;
        check(w["now"].is_number(), "now")?;
        Ok::<_, Box<dyn std::error::Error>>(w)
    }.await);
    let a_id = wa["you"].clone();

    step!("create on existing room -> taken", async {
        let mut ws = connect(&code, true, "Mallory").await?;
        expect_error(&mut ws, "taken").await?;
        expect_close(&mut ws, 4009).await
    }.await);

    let state = json!({
        "t": "state", "queue": [{ "id": "aaaaaaaaaaa" }, { "id": "bbbbbbbbbbb" }], "index": 0,
        "by": { "aaaaaaaaaaa": "Alice" }, "vid": "aaaaaaaaaaa", "pos": 1000, "playing": true
    });
    step!("host sends state alone", send(&mut a, state.clone()).await);

    // Lowercase code also tests case-insensitivity; empty name -> Guest.
    let mut b = step!("guest joins", connect(&code.to_lowercase(), false, "").await);
    let b_id = step!("late joiner gets stored state", async {
        let w = expect(&mut b, "welcome").await?;
        check(w["host"] == a_id, "host is A")?;
        check(w["members"].as_array().map(Vec::len) == Some(2), "2 members")?;
        check(w["members"][1]["name"] == "Guest", "empty name -> Guest")?;
        check(w["state"]["queue"] == state["queue"] && w["state"]["ts"].is_number(), &format!("state: {}", w["state"]))?;
        check(w["tick"]["vid"] == "aaaaaaaaaaa" && w["tick"]["pos"] == 1000 && w["tick"]["ts"].is_number(), &format!("tick: {}", w["tick"]))?;
        let m = expect(&mut a, "members").await?;
        check(m["members"].as_array().map(Vec::len) == Some(2), "host told about join")?;
        Ok::<_, Box<dyn std::error::Error>>(w["you"].clone())
    }.await);

    step!("state broadcast to others with ts", async {
        let mut s2 = state.clone();
        s2["index"] = json!(1);
        s2["vid"] = json!("bbbbbbbbbbb");
        send(&mut a, s2).await?;
        let s = expect(&mut b, "state").await?;
        check(s["index"] == 1 && s["vid"] == "bbbbbbbbbbb" && s["ts"].is_number(), &s.to_string())?;
        expect_none(&mut a, "state").await
    }.await);

    let mut c = step!("third member joins", connect(&code, false, "Carol").await);
    step!("late joiner sees latest state", async {
        let w = expect(&mut c, "welcome").await?;
        check(w["state"]["index"] == 1 && w["tick"]["vid"] == "bbbbbbbbbbb", &w.to_string())?;
        expect(&mut a, "members").await?;
        expect(&mut b, "members").await.map(|_| ())
    }.await);

    step!("tick relayed with ts", async {
        send(&mut a, json!({ "t": "tick", "vid": "bbbbbbbbbbb", "pos": 4200, "playing": false })).await?;
        for ws in [&mut b, &mut c] {
            let t = expect(ws, "tick").await?;
            check(t["pos"] == 4200 && t["playing"] == false && t["ts"].is_number(), &t.to_string())?;
        }
        expect_none(&mut a, "tick").await
    }.await);

    step!("guest tick -> denied", async {
        send(&mut b, json!({ "t": "tick", "vid": "x", "pos": 0, "playing": true })).await?;
        expect_error(&mut b, "denied").await?;
        expect_none(&mut c, "tick").await
    }.await);

    step!("req add forwarded to host", async {
        send(&mut b, json!({ "t": "req", "cmd": { "k": "add", "track": { "id": "ccccccccccc" } } })).await?;
        let r = expect(&mut a, "req").await?;
        check(r["from"] == b_id && r["name"] == "Guest" && r["cmd"]["track"]["id"] == "ccccccccccc", &r.to_string())
    }.await);

    step!("songs that aren't YouTube videos -> bad, not passed on", async {
        let mut bad = state.clone();
        bad["queue"] = json!([{ "id": "../../../x" }]);
        send(&mut a, bad).await?;
        expect_error(&mut a, "bad").await?;
        send(&mut a, json!({ "t": "tick", "vid": r"C:\Windows\x", "pos": 0, "playing": true })).await?;
        expect_error(&mut a, "bad").await?;
        send(&mut b, json!({ "t": "req", "cmd": { "k": "add", "track": { "id": r"\\host\share\x" } } })).await?;
        expect_error(&mut b, "bad").await?;
        expect_none(&mut b, "state").await?;
        expect_none(&mut a, "req").await
    }.await);

    step!("pictures from elsewhere are taken out, the song stays", async {
        let thumbs = json!([{ "url": "https://evil.example/a.jpg" }, { "url": r"file://\\host\share\a.jpg" }, { "url": "https://i.ytimg.com/vi/x/a.jpg" }]);
        send(&mut b, json!({ "t": "req", "cmd": { "k": "add", "track": { "id": "eeeeeeeeeee", "thumbs": thumbs } } })).await?;
        let r = expect(&mut a, "req").await?;
        check(r["cmd"]["track"]["thumbs"] == json!([{ "url": "https://i.ytimg.com/vi/x/a.jpg" }]), &r.to_string())
    }.await);

    step!("gated req denied while control off", async {
        send(&mut b, json!({ "t": "req", "cmd": { "k": "skip" } })).await?;
        expect_error(&mut b, "denied").await?;
        expect_none(&mut a, "req").await
    }.await);

    step!("control on broadcast, gated req then allowed", async {
        send(&mut a, json!({ "t": "control", "on": true })).await?;
        for ws in [&mut a, &mut b, &mut c] {
            let m = expect(ws, "control").await?;
            check(m["on"] == true, &m.to_string())?;
        }
        send(&mut c, json!({ "t": "req", "cmd": { "k": "seek", "pos": 9000 } })).await?;
        let r = expect(&mut a, "req").await?;
        check(r["name"] == "Carol" && r["cmd"]["pos"] == 9000, &r.to_string())
    }.await);

    step!("queue off: guest add denied, control still on", async {
        send(&mut a, json!({ "t": "control", "on": true, "queue": false })).await?;
        for ws in [&mut a, &mut b, &mut c] {
            let m = expect(ws, "control").await?;
            check(m["on"] == true && m["queue"] == false, &m.to_string())?;
        }
        send(&mut b, json!({ "t": "req", "cmd": { "k": "add", "track": { "id": "ddddddddddd" } } })).await?;
        expect_error(&mut b, "denied").await?;
        expect_none(&mut a, "req").await?;
        send(&mut b, json!({ "t": "req", "cmd": { "k": "skip" } })).await?;
        expect(&mut a, "req").await?;
        send(&mut a, json!({ "t": "control", "on": true, "queue": true })).await?;
        for ws in [&mut a, &mut b, &mut c] {
            let m = expect(ws, "control").await?;
            check(m["queue"] == true, &m.to_string())?;
        }
        R::Ok(())
    }.await);

    step!("JSON ping -> pong", async {
        send(&mut b, json!({ "t": "ping", "c": 7 })).await?;
        let p = expect(&mut b, "pong").await?;
        check(p["c"] == 7 && p["s"].is_number(), &p.to_string())
    }.await);

    step!("literal ping -> pong", async {
        b.send(Message::text("ping")).await?;
        match next_frame(&mut b, 3000).await {
            Some(Message::Text(t)) if t.as_str() == "pong" => R::Ok(()),
            other => Err(format!("got {other:?}").into()),
        }
    }.await);

    step!("oversized frame -> bad, socket stays open", async {
        let big = json!({ "t": "ping", "pad": "x".repeat(300 * 1024) });
        send(&mut b, big).await?;
        expect_error(&mut b, "bad").await?;
        send(&mut b, json!({ "t": "ping", "c": 8 })).await?;
        expect(&mut b, "pong").await.map(|_| ())
    }.await);

    step!("host leaves -> earliest member promoted", async {
        a.close(None).await?;
        for ws in [&mut b, &mut c] {
            let m = expect(ws, "members").await?;
            check(m["host"] == b_id && m["members"].as_array().map(Vec::len) == Some(2), &m.to_string())?;
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    }.await);

    step!("end -> ended for everyone, room gone", async {
        send(&mut c, json!({ "t": "end" })).await?;
        expect_error(&mut c, "denied").await?;
        send(&mut b, json!({ "t": "end" })).await?;
        for ws in [&mut b, &mut c] {
            expect(ws, "ended").await?;
            expect_close(ws, 1000).await?;
        }
        let mut late = connect(&code, false, "Late").await?;
        expect_error(&mut late, "not_found").await
    }.await);

    step!("rejoin emptied room within grace -> becomes host", async {
        let code = new_code();
        let mut ws = connect(&code, true, "Dee").await?;
        expect(&mut ws, "welcome").await?;
        send(&mut ws, json!({ "t": "bye" })).await?;
        expect_close(&mut ws, 1000).await?;
        let mut again = connect(&code, false, "Eve").await?;
        let w = expect(&mut again, "welcome").await?;
        check(w["you"] == w["host"] && w["members"].as_array().map(Vec::len) == Some(1), &w.to_string())?;
        send(&mut again, json!({ "t": "end" })).await
    }.await);

    step!("rate limit", async {
        let mut ws = connect(&new_code(), true, "Spammer").await?;
        expect(&mut ws, "welcome").await?;
        for i in 0..40 {
            send(&mut ws, json!({ "t": "ping", "c": i })).await?;
        }
        expect_error(&mut ws, "rate").await?;
        send(&mut ws, json!({ "t": "end" })).await
    }.await);

    if base().starts_with("ws://") {
        step!("stats count rooms, joins and live listeners", async {
            let before = stats().await?;
            let code = new_code();
            let mut h = connect(&code, true, "Host").await?;
            expect(&mut h, "welcome").await?;
            let mut g = connect(&code, false, "Guest").await?;
            expect(&mut g, "welcome").await?;
            let raw = http_get("/stats").await?;
            check(!raw.contains(&code), "stats must not leak room codes")?;
            let mid = stats().await?;
            let d = |s: &Value, k: &str| s["totals"][k].as_i64().unwrap_or(0);
            check(d(&mid, "rooms_created") == d(&before, "rooms_created") + 1, &mid.to_string())?;
            check(d(&mid, "guest_joins") == d(&before, "guest_joins") + 1, &mid.to_string())?;
            check(mid["live"]["rooms"].as_i64() >= Some(1) && mid["live"]["listeners"].as_i64() >= Some(2), &mid.to_string())?;
            check(mid["peaks"]["listeners"].as_i64() >= Some(2), "peak listeners")?;
            check(mid["daily"][0]["rooms"].as_i64() >= Some(1), "daily rooms")?;
            send(&mut h, json!({ "t": "end" })).await?;
            expect(&mut g, "ended").await?;
            tokio::time::sleep(Duration::from_millis(300)).await;
            let after = stats().await?;
            check(d(&after, "rooms_ended") == d(&before, "rooms_ended") + 1, &after.to_string())?;
            check(d(&after, "sessions") == d(&before, "sessions") + 1, &after.to_string())?;
            check(after["live"]["listeners"].as_i64() <= Some(mid["live"]["listeners"].as_i64().unwrap_or(0) - 2), &after.to_string())
        }.await);
    } else {
        println!("skip stats (TLS)");
    }

    println!("all checks passed");
    Ok(())
}
