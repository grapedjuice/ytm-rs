//! What does rustypipe say about a given cookie? cargo run --example cookie_check -- "<cookie>"
use rustypipe::client::RustyPipe;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cookie = std::env::args().nth(1).unwrap_or_else(|| "SID=fake; HSID=fake; SAPISID=fake".into());
    let rp = RustyPipe::builder().no_storage().no_reporter().build()?;
    match rp.user_auth_set_cookie(cookie).await {
        Ok(()) => println!("accepted"),
        Err(e) => println!("rejected: {e:?}"),
    }
    Ok(())
}
