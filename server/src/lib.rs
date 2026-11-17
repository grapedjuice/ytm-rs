//! ytm-rs Jam relay. One Durable Object per room relays small JSON control
//! messages between a host and its guests; audio never passes through here.
//! See README.md for the protocol.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use worker::*;

const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
const MAX_MEMBERS: usize = 16;
const MAX_FRAME: usize = 256 * 1024;
const GRACE_MS: i64 = 5 * 60 * 1000;
const BURST: f64 = 20.0;
const RATE_PER_SEC: f64 = 5.0;
/// How often a room with people in it reports to Stats, so a room whose
/// object died without saying so drops out of the live counts.
const BEAT_MS: i64 = 10 * 60 * 1000;

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let url = req.url()?;
    if url.path() == "/" {
        return Response::ok("ytm-rs jam server");
    }
    if url.path() == "/stats" {
        return stats_stub(&env)?.fetch_with_str("https://stats/stats").await;
    }
    let Some(raw) = url.path().strip_prefix("/jam/") else {
        return Response::error("not found", 404);
    };
    let code = raw.to_ascii_uppercase();
    if code.len() != 6 || !code.bytes().all(|b| ALPHABET.contains(&b)) {
        return Response::error("bad room code", 400);
    }
    let upgrade = req.headers().get("Upgrade")?.unwrap_or_default();
    if !upgrade.eq_ignore_ascii_case("websocket") {
        return Response::error("expected a websocket upgrade", 426);
    }
    let stub = env.durable_object("ROOMS")?.id_from_name(&code)?.get_stub()?;
    stub.fetch_with_request(req).await
}

/// Room-wide settings, kept in storage under "meta".
#[derive(Serialize, Deserialize, Default)]
struct Meta {
    host: String,
    control: bool,
    /// Guests may not add songs (they may by default).
    #[serde(default)]
    no_queue: bool,
    /// Join counter, so "earliest joined" is unambiguous.
    seq: u32,
    #[serde(default)]
    empty_since: Option<f64>,
}

/// Per-socket data, kept in the socket attachment so it survives hibernation.
#[derive(Serialize, Deserialize)]
struct Member {
    id: String,
    name: String,
    joined: u32,
    // token bucket for rate limiting
    tokens: f64,
    last: f64,
    /// Set once we've processed this socket's departure.
    #[serde(default)]
    gone: bool,
}

#[durable_object]
pub struct Room {
    state: State,
    env: Env,
}

impl DurableObject for Room {
    fn new(state: State, env: Env) -> Self {
        // Keepalives are answered by the runtime without waking us up.
        if let Ok(pair) = WebSocketRequestResponsePair::new("ping", "pong") {
            state.set_websocket_auto_response(&pair);
        }
        Self { state, env }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        let url = req.url()?;
        let param = |k: &str| url.query_pairs().find(|(n, _)| n == k).map(|(_, v)| v.into_owned());
        let create = param("create");
        let pair = WebSocketPair::new()?;
        if !matches!(create.as_deref(), Some("0" | "1")) || param("v").is_some_and(|v| v != "1") {
            return self.reject(pair, "bad", "expected create=0|1 and v=1");
        }

        let storage = self.state.storage();
        let now = now();
        let members = self.members(None);
        let mut meta = match storage.get::<Meta>("meta").await? {
            _ if create.as_deref() == Some("1") => {
                if !members.is_empty() {
                    return self.reject(pair, "taken", "that room code is in use");
                }
                // Nobody is here, so whatever an abandoned room left behind can go.
                storage.delete_all().await?;
                Meta::default()
            }
            Some(m) => {
                let expired = members.is_empty()
                    && m.empty_since.is_some_and(|t| now - t >= GRACE_MS as f64);
                if expired {
                    return self.reject(pair, "not_found", "no such room");
                }
                if members.len() >= MAX_MEMBERS {
                    return self.reject(pair, "full", "the room is full");
                }
                m
            }
            None => return self.reject(pair, "not_found", "no such room"),
        };

        meta.seq += 1;
        let me = Member {
            id: random_id(),
            name: clean_name(&param("name").unwrap_or_default()),
            joined: meta.seq,
            tokens: BURST,
            last: now,
            gone: false,
        };
        // First one in (or back into an emptied room) hosts.
        if members.is_empty() {
            meta.host = me.id.clone();
        }
        meta.empty_since = None;
        storage.put("meta", &meta).await?;
        storage.set_alarm(BEAT_MS).await?;

        let ws = pair.server;
        self.state.accept_web_socket(&ws);
        ws.serialize_attachment(&me)?;

        let mut list = member_list(&members);
        list.push(json!({ "id": me.id, "name": me.name }));
        let stored = |s: Option<String>| s.and_then(|s| serde_json::from_str::<Value>(&s).ok());
        send(&ws, &json!({
            "t": "welcome", "you": me.id, "host": meta.host, "control": meta.control, "queue": !meta.no_queue,
            "members": list, "now": now as u64,
            "state": stored(storage.get("state").await?),
            "tick": stored(storage.get("tick").await?),
        }));
        // The newcomer already has the list in its welcome.
        let update = json!({ "t": "members", "host": meta.host, "members": list });
        for (other, _) in &members {
            send(other, &update);
        }
        let ev = if create.as_deref() == Some("1") { "create" } else { "join" };
        self.report(ev, list.len()).await;
        Response::from_websocket(pair.client)
    }

    async fn websocket_message(&self, ws: WebSocket, msg: WebSocketIncomingMessage) -> Result<()> {
        let Some(mut me) = ws.deserialize_attachment::<Member>()? else { return Ok(()) };
        if me.gone {
            return Ok(());
        }
        let now = now();
        me.tokens = (me.tokens + (now - me.last) / 1000.0 * RATE_PER_SEC).min(BURST);
        me.last = now;
        let limited = me.tokens < 1.0;
        if !limited {
            me.tokens -= 1.0;
        }
        ws.serialize_attachment(&me)?;
        if limited {
            return error(&ws, "rate", "slow down");
        }

        let WebSocketIncomingMessage::String(text) = msg else {
            return error(&ws, "bad", "binary frames are not supported");
        };
        if text.len() > MAX_FRAME {
            return error(&ws, "bad", "frame too large");
        }
        let Ok(Value::Object(mut m)) = serde_json::from_str::<Value>(&text) else {
            return error(&ws, "bad", "expected a JSON object");
        };
        let t = m.remove("t").and_then(|t| t.as_str().map(String::from)).unwrap_or_default();

        let storage = self.state.storage();
        let Some(mut meta) = storage.get::<Meta>("meta").await? else { return Ok(()) };
        let is_host = meta.host == me.id;
        if matches!(t.as_str(), "state" | "tick" | "control" | "end") && !is_host {
            return error(&ws, "denied", "only the host can do that");
        }

        match t.as_str() {
            "ping" => send(&ws, &json!({ "t": "pong", "c": m.get("c"), "s": now as u64 })),
            "state" => {
                if !valid_state(&mut m) {
                    return error(&ws, "bad", "malformed state");
                }
                m.insert("ts".into(), (now as u64).into());
                // A full state also resets the playhead late joiners see.
                let tick = json!({ "vid": m["vid"], "pos": m["pos"], "playing": m["playing"], "ts": now as u64 });
                storage.put("state", serde_json::to_string(&m)?).await?;
                storage.put("tick", tick.to_string()).await?;
                m.insert("t".into(), "state".into());
                self.broadcast(&Value::Object(m), Some(&ws));
            }
            "tick" => {
                if !valid_tick(&m) {
                    return error(&ws, "bad", "malformed tick");
                }
                m.insert("ts".into(), (now as u64).into());
                storage.put("tick", serde_json::to_string(&m)?).await?;
                m.insert("t".into(), "tick".into());
                self.broadcast(&Value::Object(m), Some(&ws));
            }
            "control" => {
                let Some(on) = m.get("on").and_then(Value::as_bool) else {
                    return error(&ws, "bad", "control needs on: bool");
                };
                meta.control = on;
                // Optional: older clients only send `on`.
                if let Some(queue) = m.get("queue").and_then(Value::as_bool) {
                    meta.no_queue = !queue;
                }
                storage.put("meta", &meta).await?;
                self.broadcast(&json!({ "t": "control", "on": on, "queue": !meta.no_queue }), None);
            }
            "end" => {
                storage.delete_alarm().await?;
                storage.delete_all().await?;
                for (other, mut member) in self.members(None) {
                    member.gone = true;
                    let _ = other.serialize_attachment(&member);
                    send(&other, &json!({ "t": "ended" }));
                    let _ = other.close(Some(1000), Some("ended"));
                }
                self.report("end", 0).await;
            }
            "req" => {
                let mut cmd = m.remove("cmd").unwrap_or(Value::Null);
                let Some(kind) = valid_cmd(&mut cmd) else {
                    return error(&ws, "bad", "malformed req");
                };
                let gated = matches!(kind, "toggle" | "skip" | "prev" | "seek" | "jump");
                if gated && !meta.control && !is_host {
                    return error(&ws, "denied", "Only the host can control playback");
                }
                if !gated && meta.no_queue && !is_host {
                    return error(&ws, "denied", "The host turned off adding songs");
                }
                let fwd = json!({ "t": "req", "from": me.id, "name": me.name, "cmd": cmd });
                if let Some((host, _)) = self.members(None).iter().find(|(_, x)| x.id == meta.host) {
                    send(host, &fwd);
                }
            }
            "bye" => {
                self.leave(&ws).await?;
                let _ = ws.close(Some(1000), Some("bye"));
            }
            _ => return error(&ws, "bad", "unknown message type"),
        }
        Ok(())
    }

    async fn websocket_close(&self, ws: WebSocket, _code: usize, _reason: String, _clean: bool) -> Result<()> {
        self.leave(&ws).await?;
        // Complete the close handshake; harmless if the runtime already did.
        let _ = ws.close(Some(1000), Some("bye"));
        Ok(())
    }

    async fn websocket_error(&self, ws: WebSocket, _error: Error) -> Result<()> {
        self.leave(&ws).await
    }

    async fn alarm(&self) -> Result<Response> {
        let members = self.members(None).len();
        if members > 0 {
            // Still going: the alarm is the heartbeat.
            self.report("beat", members).await;
            self.state.storage().set_alarm(BEAT_MS).await?;
        } else {
            // Nobody came back during the grace period: forget the room.
            self.state.storage().delete_all().await?;
        }
        Response::ok("ok")
    }
}

impl Room {
    /// Refuse a connection: accept it just long enough to say why, then close.
    /// Hibernation-accepted with no attachment, so it never counts as a member
    /// and the runtime cleans up quietly if the client drops mid-close.
    fn reject(&self, pair: WebSocketPair, code: &str, msg: &str) -> Result<Response> {
        let close_code = match code {
            "not_found" => 4004,
            "taken" => 4009,
            "full" => 4029,
            _ => 4000,
        };
        self.state.accept_web_socket(&pair.server);
        send(&pair.server, &json!({ "t": "error", "code": code, "msg": msg }));
        pair.server.close(Some(close_code), Some(code))?;
        Response::from_websocket(pair.client)
    }

    /// Live members in join order, optionally leaving one socket out.
    fn members(&self, skip: Option<&WebSocket>) -> Vec<(WebSocket, Member)> {
        let mut out: Vec<_> = self
            .state
            .get_websockets()
            .into_iter()
            .filter(|w| Some(w) != skip)
            .filter_map(|w| {
                let m = w.deserialize_attachment::<Member>().ok().flatten()?;
                (!m.gone).then_some((w, m))
            })
            .collect();
        out.sort_by_key(|(_, m)| m.joined);
        out
    }

    /// Tells Stats how many people this room has now, and logs the event for
    /// Workers Logs. Metrics are best effort: a failure never breaks the room.
    async fn report(&self, ev: &str, members: usize) {
        let Some(code) = self.state.id().name() else { return };
        console_log!("{}", json!({ "jam": ev, "room": code, "members": members }));
        let body = json!({ "code": code, "ev": ev, "members": members }).to_string();
        let mut init = RequestInit::new();
        init.with_method(Method::Post).with_body(Some(body.into()));
        let sent = async {
            let req = Request::new_with_init("https://stats/report", &init)?;
            stats_stub(&self.env)?.fetch_with_request(req).await
        };
        if let Err(e) = sent.await {
            console_error!("stats report failed: {e}");
        }
    }

    fn broadcast(&self, msg: &Value, skip: Option<&WebSocket>) {
        for (ws, _) in self.members(skip) {
            send(&ws, msg);
        }
    }

    /// Runs once per socket, whether it said bye, closed, or errored.
    async fn leave(&self, ws: &WebSocket) -> Result<()> {
        let Some(mut me) = ws.deserialize_attachment::<Member>()? else { return Ok(()) };
        if me.gone {
            return Ok(());
        }
        me.gone = true;
        let _ = ws.serialize_attachment(&me);

        let storage = self.state.storage();
        let Some(mut meta) = storage.get::<Meta>("meta").await? else { return Ok(()) };
        let rest = self.members(Some(ws));
        self.report("leave", rest.len()).await;
        if rest.is_empty() {
            meta.empty_since = Some(now());
            storage.put("meta", &meta).await?;
            storage.set_alarm(GRACE_MS).await?;
            return Ok(());
        }
        if meta.host == me.id {
            meta.host = rest[0].1.id.clone();
            storage.put("meta", &meta).await?;
        }
        let update = json!({ "t": "members", "host": meta.host, "members": member_list(&rest) });
        for (other, _) in &rest {
            send(other, &update);
        }
        Ok(())
    }
}

fn now() -> f64 {
    Date::now().as_millis() as f64
}

fn send(ws: &WebSocket, msg: &Value) {
    // A socket that's mid-close can refuse sends; that's fine.
    let _ = ws.send_with_str(msg.to_string());
}

fn error(ws: &WebSocket, code: &str, msg: &str) -> Result<()> {
    send(ws, &json!({ "t": "error", "code": code, "msg": msg }));
    Ok(())
}

fn member_list(members: &[(WebSocket, Member)]) -> Vec<Value> {
    members.iter().map(|(_, m)| json!({ "id": m.id, "name": m.name })).collect()
}

fn random_id() -> String {
    (0..8)
        .map(|_| ALPHABET[(js_sys::Math::random() * ALPHABET.len() as f64) as usize] as char)
        .collect()
}

/// Trimmed, control characters stripped, at most 32 chars, "Guest" if empty.
fn clean_name(raw: &str) -> String {
    let name: String = raw.chars().filter(|c| !c.is_control()).collect();
    let name: String = name.trim().chars().take(32).collect();
    match name.trim_end() {
        "" => "Guest".into(),
        n => n.into(),
    }
}

fn is_num(v: Option<&Value>) -> bool {
    v.is_some_and(Value::is_number)
}

fn valid_tick(m: &Map<String, Value>) -> bool {
    m.get("vid").and_then(Value::as_str).is_some_and(|v| v.is_empty() || is_video_id(v))
        && is_num(m.get("pos"))
        && m.get("playing").is_some_and(Value::is_boolean)
}

/// Every song in the queue is checked (see `clean_track`).
fn valid_state(m: &mut Map<String, Value>) -> bool {
    valid_tick(m)
        && m.get_mut("queue").and_then(Value::as_array_mut).is_some_and(|q| q.iter_mut().all(clean_track))
        && is_num(m.get("index"))
        && m.get("by").is_some_and(Value::is_object)
}

/// Returns the command kind if the command is well formed.
fn valid_cmd(cmd: &mut Value) -> Option<&str> {
    let ok = match cmd.get("k")?.as_str()? {
        "add" | "next" => cmd.get_mut("track").is_some_and(clean_track),
        "seek" => is_num(cmd.get("pos")),
        "jump" => is_num(cmd.get("index")),
        "toggle" | "skip" | "prev" => true,
        _ => false,
    };
    if ok { cmd.get("k")?.as_str() } else { None }
}

/// A song as members may pass it on: its ID (and the one it stands for, if any) a YouTube
/// video ID, since clients put it in file names. Pictures from anywhere but YouTube's own
/// image hosts over https are taken out, as clients fetch them.
fn clean_track(t: &mut Value) -> bool {
    let Some(t) = t.as_object_mut() else { return false };
    let id_ok = |v: &Value| v.as_str().is_some_and(is_video_id);
    if !t.get("id").is_some_and(id_ok) || t.get("stands_for").is_some_and(|v| !v.is_null() && !id_ok(v)) {
        return false;
    }
    if let Some(Value::Array(thumbs)) = t.get_mut("thumbs") {
        thumbs.retain(|th| th.get("url").and_then(Value::as_str).is_some_and(youtube_image));
    }
    true
}

/// 11 letters, digits, `-` or `_`.
fn is_video_id(id: &str) -> bool {
    id.len() == 11 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// An https link to one of the hosts YouTube's pictures come from.
fn youtube_image(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else { return false };
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let ours = |domain: &str| host == domain || host.strip_suffix(domain).is_some_and(|sub| sub.ends_with('.'));
    host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-') && (ours("ytimg.com") || ours("googleusercontent.com") || ours("ggpht.com"))
}

fn stats_stub(env: &Env) -> Result<Stub> {
    env.durable_object("STATS")?.id_from_name("global")?.get_stub()
}

/// A room that hasn't reported in this long is presumed gone (rooms with
/// people in them report every BEAT_MS).
const STALE_MS: i64 = 25 * 60 * 1000;

/// One object for the whole service: rooms report joins and leaves here, and
/// `GET /stats` reads it. Holds counts only, never room codes or names.
#[durable_object]
pub struct Stats {
    sql: SqlStorage,
}

#[derive(Deserialize)]
struct Report {
    code: String,
    ev: String,
    members: u32,
}

#[derive(Deserialize)]
struct Num {
    v: Option<f64>,
}

#[derive(Deserialize)]
struct Day {
    day: String,
    rooms: f64,
    joins: f64,
    peak_listeners: f64,
}

impl DurableObject for Stats {
    fn new(state: State, _env: Env) -> Self {
        let sql = state.storage().sql();
        for q in [
            "CREATE TABLE IF NOT EXISTS live (code TEXT PRIMARY KEY, members INTEGER NOT NULL, started INTEGER NOT NULL, seen INTEGER NOT NULL)",
            "CREATE TABLE IF NOT EXISTS counters (k TEXT PRIMARY KEY, v INTEGER NOT NULL)",
            "CREATE TABLE IF NOT EXISTS daily (day TEXT PRIMARY KEY, rooms INTEGER NOT NULL DEFAULT 0, joins INTEGER NOT NULL DEFAULT 0, peak_listeners INTEGER NOT NULL DEFAULT 0)",
        ] {
            if let Err(e) = sql.exec(q, None) {
                console_error!("stats schema: {e}");
            }
        }
        Self { sql }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        let now = now() as i64;
        self.sql.exec("DELETE FROM live WHERE seen < ?", vec![(now - STALE_MS).into()])?;
        match req.path().as_str() {
            "/report" => {
                let r: Report = req.json().await?;
                self.record(&r, now)?;
                Response::ok("ok")
            }
            "/stats" => {
                let mut res = Response::from_json(&self.snapshot(now)?)?;
                res.headers_mut().set("Cache-Control", "no-store")?;
                res.headers_mut().set("Access-Control-Allow-Origin", "*")?;
                Ok(res)
            }
            _ => Response::error("not found", 404),
        }
    }
}

impl Stats {
    fn record(&self, r: &Report, now: i64) -> Result<()> {
        let day = day_of(now);
        let code = r.code.as_str();
        let started = self.num("SELECT started AS v FROM live WHERE code = ?", vec![code.into()])?;
        if r.members == 0 {
            // A stretch with people in it just ended.
            if let Some(started) = started {
                self.sql.exec("DELETE FROM live WHERE code = ?", vec![code.into()])?;
                self.bump("sessions", 1)?;
                self.bump("session_ms", now - started as i64)?;
            }
        } else {
            self.sql.exec(
                "INSERT INTO live (code, members, started, seen) VALUES (?, ?, ?, ?)
                 ON CONFLICT(code) DO UPDATE SET members = excluded.members, seen = excluded.seen",
                vec![code.into(), (r.members as i64).into(), now.into(), now.into()],
            )?;
        }
        self.sql.exec("INSERT OR IGNORE INTO daily (day) VALUES (?)", vec![day.as_str().into()])?;
        self.sql.exec("INSERT OR IGNORE INTO counters (k, v) VALUES ('since', ?)", vec![now.into()])?;
        match r.ev.as_str() {
            "create" => {
                self.bump("rooms_created", 1)?;
                self.bump("joins", 1)?;
                self.sql.exec("UPDATE daily SET rooms = rooms + 1, joins = joins + 1 WHERE day = ?", vec![day.as_str().into()])?;
            }
            "join" => {
                self.bump("joins", 1)?;
                self.bump("guest_joins", 1)?;
                self.sql.exec("UPDATE daily SET joins = joins + 1 WHERE day = ?", vec![day.as_str().into()])?;
            }
            "end" => self.bump("rooms_ended", 1)?,
            _ => {}
        }
        let (rooms, listeners) = self.live()?;
        self.peak("peak_rooms", rooms)?;
        self.peak("peak_listeners", listeners)?;
        self.sql.exec(
            "UPDATE daily SET peak_listeners = max(peak_listeners, ?) WHERE day = ?",
            vec![listeners.into(), day.as_str().into()],
        )?;
        Ok(())
    }

    fn snapshot(&self, now: i64) -> Result<Value> {
        let c = |k: &str| self.num("SELECT v FROM counters WHERE k = ?", vec![k.into()]).map(|v| v.unwrap_or(0.0) as i64);
        let (rooms, listeners) = self.live()?;
        let largest = self.num("SELECT max(members) AS v FROM live", None)?.unwrap_or(0.0) as i64;
        let (sessions, session_ms) = (c("sessions")?, c("session_ms")?);
        let avg_min = if sessions > 0 { (session_ms as f64 / sessions as f64 / 60_000.0 * 10.0).round() / 10.0 } else { 0.0 };
        let daily: Vec<Day> = self
            .sql
            .exec("SELECT day, rooms, joins, peak_listeners FROM daily ORDER BY day DESC LIMIT 30", None)?
            .to_array()?;
        let since = c("since")?;
        Ok(json!({
            "live": { "rooms": rooms, "listeners": listeners, "largest_room": largest },
            "totals": {
                "rooms_created": c("rooms_created")?,
                "joins": c("joins")?,
                "guest_joins": c("guest_joins")?,
                "rooms_ended": c("rooms_ended")?,
                "sessions": sessions,
                "avg_session_min": avg_min,
            },
            "peaks": { "rooms": c("peak_rooms")?, "listeners": c("peak_listeners")? },
            "daily": daily.iter().map(|d| json!({
                "day": d.day, "rooms": d.rooms as i64, "joins": d.joins as i64, "peak_listeners": d.peak_listeners as i64,
            })).collect::<Vec<_>>(),
            "since": (since > 0).then_some(since),
            "now": now,
        }))
    }

    /// Rooms with people in them, and how many people that is.
    fn live(&self) -> Result<(i64, i64)> {
        let rooms = self.num("SELECT count(*) AS v FROM live", None)?.unwrap_or(0.0) as i64;
        let listeners = self.num("SELECT sum(members) AS v FROM live", None)?.unwrap_or(0.0) as i64;
        Ok((rooms, listeners))
    }

    fn num(&self, q: &str, binds: impl Into<Option<Vec<SqlStorageValue>>>) -> Result<Option<f64>> {
        Ok(self.sql.exec(q, binds)?.to_array::<Num>()?.into_iter().next().and_then(|n| n.v))
    }

    fn bump(&self, k: &str, by: i64) -> Result<()> {
        self.sql.exec(
            "INSERT INTO counters (k, v) VALUES (?, ?) ON CONFLICT(k) DO UPDATE SET v = v + excluded.v",
            vec![k.into(), by.into()],
        )?;
        Ok(())
    }

    fn peak(&self, k: &str, val: i64) -> Result<()> {
        self.sql.exec(
            "INSERT INTO counters (k, v) VALUES (?, ?) ON CONFLICT(k) DO UPDATE SET v = max(v, excluded.v)",
            vec![k.into(), val.into()],
        )?;
        Ok(())
    }
}

/// "YYYY-MM-DD" in UTC.
fn day_of(ms: i64) -> String {
    let iso: String = js_sys::Date::new(&(ms as f64).into()).to_iso_string().into();
    iso[..10].to_string()
}
