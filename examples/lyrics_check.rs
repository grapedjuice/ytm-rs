//! Parse a saved Better Lyrics SSE body and summarise the chosen result.
//! cargo run --example lyrics_check -- <sse.txt>...
#[path = "../src/lyrics.rs"]
#[allow(dead_code)]
mod lyrics;
#[allow(dead_code)]
mod turnstile {
    pub fn obtain(_: std::path::PathBuf) -> anyhow::Result<String> { unimplemented!() }
}

fn main() {
    for f in std::env::args().skip(1) {
        let body = std::fs::read_to_string(&f).unwrap();
        match lyrics::best_from_stream(&body) {
            Some(l) => {
                let words: usize = l.lines.iter().map(|x| x.words.len()).sum();
                let bg = l.lines.iter().filter(|x| x.background).count();
                let alt = l.lines.iter().filter(|x| x.alt_singer).count();
                println!("{f}: {} {:?}, {} lines, {words} timed words, {bg} bg, {alt} alt-singer, first at {}ms",
                    l.source, l.sync, l.lines.len(), l.lines[0].start);
            }
            None => println!("{f}: none"),
        }
    }
}
