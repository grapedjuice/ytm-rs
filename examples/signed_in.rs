//! Signed-in playback path: Premium check, web music player, QuickJS challenge solving.
//! cargo run --example signed_in -- <videoId>...
#[path = "../src/innertube.rs"]
#[allow(dead_code)]
mod innertube;
#[path = "../src/jsc.rs"]
#[allow(dead_code)]
mod jsc;
#[path = "../src/webmusic.rs"]
mod webmusic;

use std::time::Instant;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let data = directories::ProjectDirs::from("", "", "ytm-rs").unwrap().data_dir().to_owned();
    let http = reqwest::Client::new();
    let session = webmusic::Session::load(&data).expect("not signed in");
    let solver = jsc::Solver::new(&data);
    let t = Instant::now();
    println!("premium {} ({:?})", session.is_premium(&http).await?, t.elapsed());
    for id in std::env::args().skip(1) {
        let t = Instant::now();
        let s = session.audio_stream(&http, &solver, &id).await?;
        println!("{id}: resolved {} bytes in {:?}", s.size, t.elapsed());
        // Past the ~1 MiB point where unauthorised streams get cut off.
        let from = (s.size / 2).max(1 << 20).min(s.size - 1);
        let r = http.get(format!("{}&range={from}-{}", s.url, (from + 65535).min(s.size - 1))).header("User-Agent", webmusic::UA).send().await?;
        println!("{id}: range at {from} -> {} ({} bytes)", r.status(), r.bytes().await?.len());
    }
    Ok(())
}
