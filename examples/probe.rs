//! Headless smoke test of the playback path:
//! search → visionOS player → progressive ranged download → AAC decode → seek.
//! cargo run --release --example probe -- "query"
#[path = "../src/innertube.rs"]
mod innertube;
#[path = "../src/stream.rs"]
mod stream;

use rodio::Source;
use rustypipe::client::RustyPipe;
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let q = std::env::args().nth(1).unwrap_or_else(|| "daft punk get lucky".into());
    let rp = RustyPipe::builder().no_storage().build()?;
    let http = reqwest::Client::new();

    let t = Instant::now();
    let res = rp.query().music_search_tracks(&q).await?;
    let track = res.items.items.first().expect("no results").clone();
    println!("search     {:>6} ms  {} ({}) {:?}s", t.elapsed().as_millis(), track.name, track.id, track.duration);

    let t = Instant::now();
    let vd = rp.query().get_visitor_data(false).await?;
    let s = innertube::audio_stream(&http, &track.id, &vd).await?;
    println!("resolve    {:>6} ms  {} bytes", t.elapsed().as_millis(), s.size);

    let shared = stream::Shared::new(s.size);
    let sh = shared.clone();
    let t = Instant::now();
    let td = Instant::now();
    tokio::spawn(async move {
        stream::download(http, s.url.clone(), innertube::UA.into(), sh, None).await;
        println!("download   {:>6} ms  complete", td.elapsed().as_millis());
    });
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        // Fast start: non-seekable decoder (what the app plays first).
        let mut dec = stream::decoder(shared.clone(), false)?;
        println!("decoder    {:>6} ms  {} Hz x{} (non-seekable)", t.elapsed().as_millis(), dec.sample_rate().get(), dec.channels().get());
        let per_sec = dec.sample_rate().get() as usize * dec.channels().get() as usize;
        let n = dec.by_ref().take(per_sec * 5).count();
        println!("5s decode  {:>6} ms  {n} samples", t.elapsed().as_millis());
        // Seek path: second, seekable decoder over the same buffer (what the app swaps in).
        let mut seekable = stream::decoder(shared, true)?;
        seekable.try_seek(Duration::from_secs(150)).map_err(|e| anyhow::anyhow!("{e}"))?;
        let n = seekable.by_ref().take(per_sec).count();
        println!("seek 2:30  {:>6} ms  {n} samples (seekable decoder)", t.elapsed().as_millis());
        drop(dec);
        let rest = seekable.count();
        println!("to end     {:>6} ms  ~{}s more", t.elapsed().as_millis(), rest / per_sec);
        Ok(())
    })
    .await??;
    Ok(())
}
