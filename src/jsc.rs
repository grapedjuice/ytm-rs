//! Solves the challenges on signed-in stream URLs (the `sig` signature and the `n`
//! throttling parameter) with yt-dlp's EJS solver, run in QuickJS: the engine rustypipe
//! already links, so no Node or yt-dlp process is involved.
//!
//! The player script is preprocessed once per player version and cached on disk.
//! A worker thread keeps the current one loaded and exits after five idle minutes to
//! give the memory back.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crossbeam_channel::{RecvTimeoutError, Sender};
use rquickjs::{Context, Runtime};
use tokio::sync::oneshot;

/// yt-dlp/ejs 0.8.0 (Unlicense; bundles meriyah, ISC, and astring, MIT).
const LIB: &str = include_str!("../assets/ejs/yt.solver.lib.min.js");
const CORE: &str = include_str!("../assets/ejs/yt.solver.core.min.js");
const IDLE: Duration = Duration::from_secs(300);
/// YouTube rolls out new player versions every few days.
const PLAYER_TTL: Duration = Duration::from_secs(3600);

#[derive(Clone)]
pub struct Player {
    pub id: String,
    pub sts: u32,
}

type Reply<T> = oneshot::Sender<anyhow::Result<T>>;

enum Job {
    Preprocess { base: String, reply: Reply<String> },
    Solve { path: PathBuf, n: Option<String>, sig: Option<String>, reply: Reply<(Option<String>, Option<String>)> },
}

pub struct Solver {
    dir: PathBuf,
    worker: Mutex<Option<Sender<Job>>>,
    player: tokio::sync::Mutex<Option<(Player, Instant)>>,
}

impl Solver {
    pub fn new(data_dir: &Path) -> Self {
        Self { dir: data_dir.join("ejs"), worker: Mutex::new(None), player: tokio::sync::Mutex::new(None) }
    }

    /// The current player version, preprocessed and ready to solve with.
    pub async fn player(&self, http: &reqwest::Client) -> anyhow::Result<Player> {
        let mut cached = self.player.lock().await;
        if let Some((p, _)) = cached.as_ref().filter(|(_, at)| at.elapsed() < PLAYER_TTL) {
            return Ok(p.clone());
        }
        let iframe = http.get("https://www.youtube.com/iframe_api").send().await?.error_for_status()?.text().await?;
        let id = player_id(&iframe).ok_or_else(|| anyhow::anyhow!("no player version in iframe_api"))?;
        let script = self.dir.join(format!("{id}.js"));
        let sts = match std::fs::read_to_string(self.dir.join(format!("{id}.sts"))).ok().and_then(|s| s.trim().parse().ok()) {
            Some(sts) if script.exists() => sts,
            _ => {
                let t = Instant::now();
                let url = format!("https://www.youtube.com/s/player/{id}/player_ias.vflset/en_US/base.js");
                let base = http.get(url).send().await?.error_for_status()?.text().await?;
                let sts = signature_timestamp(&base).ok_or_else(|| anyhow::anyhow!("no signatureTimestamp in player {id}"))?;
                let (reply, rx) = oneshot::channel();
                self.send(Job::Preprocess { base, reply })?;
                let pre = rx.await??;
                // Keep only the current player: each is ~4 MB.
                if let Ok(old) = std::fs::read_dir(&self.dir) {
                    for f in old.flatten() {
                        let _ = std::fs::remove_file(f.path());
                    }
                }
                std::fs::create_dir_all(&self.dir)?;
                std::fs::write(&script, pre)?;
                std::fs::write(self.dir.join(format!("{id}.sts")), sts.to_string())?;
                log::info!("player {id} preprocessed in {:?}", t.elapsed());
                sts
            }
        };
        let p = Player { id, sts };
        *cached = Some((p.clone(), Instant::now()));
        Ok(p)
    }

    /// Prepare the current player and load it into the worker ahead of the first song.
    pub async fn warm(&self, http: &reqwest::Client) -> anyhow::Result<()> {
        let player = self.player(http).await?;
        self.solve(&player, None, None).await.map(|_| ())
    }

    pub async fn solve(&self, player: &Player, n: Option<String>, sig: Option<String>) -> anyhow::Result<(Option<String>, Option<String>)> {
        let (reply, rx) = oneshot::channel();
        self.send(Job::Solve { path: self.dir.join(format!("{}.js", player.id)), n, sig, reply })?;
        rx.await?
    }

    /// Hand a job to the worker, starting it if it has exited.
    fn send(&self, mut job: Job) -> anyhow::Result<()> {
        let mut worker = self.worker.lock().unwrap();
        if let Some(tx) = worker.as_ref() {
            match tx.send(job) {
                Ok(()) => return Ok(()),
                Err(e) => job = e.0, // it went idle and exited
            }
        }
        let (tx, rx) = crossbeam_channel::unbounded();
        std::thread::Builder::new().name("jsc".into()).spawn(move || {
            let mut loaded: Option<(PathBuf, Runtime, Context)> = None;
            loop {
                match rx.recv_timeout(IDLE) {
                    Ok(Job::Preprocess { base, reply }) => {
                        let _ = reply.send(preprocess(&base));
                    }
                    Ok(Job::Solve { path, n, sig, reply }) => {
                        if loaded.as_ref().is_none_or(|(p, ..)| *p != path) {
                            loaded = None;
                            match load(&path) {
                                Ok((rt, ctx)) => loaded = Some((path, rt, ctx)),
                                Err(e) => {
                                    let _ = reply.send(Err(e));
                                    continue;
                                }
                            }
                        }
                        let (.., ctx) = loaded.as_ref().unwrap();
                        let _ = reply.send(solve(ctx, n, sig));
                    }
                    Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => return,
                }
            }
        })?;
        tx.send(job).map_err(|_| anyhow::anyhow!("solver thread failed to start"))?;
        *worker = Some(tx);
        Ok(())
    }
}

fn js_err(ctx: &rquickjs::Ctx, e: rquickjs::Error) -> anyhow::Error {
    if matches!(e, rquickjs::Error::Exception) {
        let ex = ctx.catch();
        let msg = ex.as_exception().map(|x| x.to_string()).or_else(|| ex.as_string().and_then(|s| s.to_string().ok()));
        anyhow::anyhow!("js: {}", msg.unwrap_or_else(|| "exception".into()))
    } else {
        anyhow::anyhow!("js: {e}")
    }
}

/// Parse the player and extract its solver functions into a standalone script.
fn preprocess(base: &str) -> anyhow::Result<String> {
    let rt = Runtime::new()?;
    rt.set_max_stack_size(64 << 20);
    let ctx = Context::full(&rt)?;
    ctx.with(|ctx| {
        let input = serde_json::json!({"type": "player", "player": base, "output_preprocessed": true, "requests": []});
        let src = format!("{LIB}\nObject.assign(globalThis, lib);\n{CORE}\nJSON.stringify(jsc({input}))");
        let out: String = ctx.eval(src).map_err(|e| js_err(&ctx, e))?;
        let v: serde_json::Value = serde_json::from_str(&out)?;
        v["preprocessed_player"].as_str().map(str::to_owned).ok_or_else(|| anyhow::anyhow!("solver returned no player: {out:.200}"))
    })
}

fn load(path: &Path) -> anyhow::Result<(Runtime, Context)> {
    let pre = std::fs::read_to_string(path)?;
    let rt = Runtime::new()?;
    rt.set_max_stack_size(64 << 20);
    let ctx = Context::full(&rt)?;
    ctx.with(|ctx| {
        let src = format!("var _r = {{n: null, sig: null}}; Function('_result', {})(_r);", serde_json::to_string(&pre)?);
        ctx.eval::<(), _>(src).map_err(|e| js_err(&ctx, e))
    })?;
    Ok((rt, ctx))
}

fn solve(ctx: &Context, n: Option<String>, sig: Option<String>) -> anyhow::Result<(Option<String>, Option<String>)> {
    ctx.with(|ctx| {
        let run = |kind: &str, input: Option<String>| -> anyhow::Result<Option<String>> {
            let Some(input) = input else { return Ok(None) };
            let src = format!("_r.{kind}({})", serde_json::to_string(&input)?);
            let out: String = ctx.eval(src).map_err(|e| js_err(&ctx, e))?;
            Ok(Some(out))
        };
        Ok((run("n", n)?, run("sig", sig)?))
    })
}

/// `player\/5203c085\/` in iframe_api's script.
fn player_id(iframe: &str) -> Option<String> {
    iframe.match_indices("player").find_map(|(i, _)| {
        let rest = iframe[i + 6..].trim_start_matches(['\\', '/']);
        let id = rest.get(..8)?;
        (id.bytes().all(|b| b.is_ascii_hexdigit()) && rest[8..].starts_with(['\\', '/'])).then(|| id.to_owned())
    })
}

/// `signatureTimestamp:20732` (or `sts:20732`) in the player script.
fn signature_timestamp(base: &str) -> Option<u32> {
    ["signatureTimestamp", "sts"].iter().find_map(|key| {
        base.match_indices(key).find_map(|(i, _)| {
            let rest = base[i + key.len()..].trim_start().strip_prefix(':')?.trim_start();
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            (digits.len() == 5).then(|| digits.parse().ok()).flatten()
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_player_id_and_sts() {
        assert_eq!(player_id(r"var a='https:\/\/www.youtube.com\/s\/player\/5203c085\/www-widgetapi.vflset'").as_deref(), Some("5203c085"));
        assert_eq!(signature_timestamp("x={a:1,signatureTimestamp:20732,b:2}"), Some(20732));
        assert_eq!(signature_timestamp("sts:123,sts:20733"), Some(20733));
    }
}
