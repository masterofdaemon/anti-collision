use std::env;
use std::time::Duration;
use tokio::sync::watch;
use chrono::Local;

use anti_collision::core::{Config, run_monitor_loop};
use std::sync::Arc;

fn log(msg: &str) {
    println!("[{}] {}", Local::now().format("%Y-%m-%d %H:%M:%S"), msg);
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let default_url = "https://speed.cloudflare.com/__down?bytes=100000000";
    let url = args.get(1).map(|s| s.as_str()).unwrap_or(default_url);

    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .tcp_keepalive(Duration::from_secs(60))
        .build()?;

    let config = Config::default();
    let (_stop_tx, stop_rx) = watch::channel(false);
    let logger: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(log);
    run_monitor_loop(url, &client, &config, stop_rx, logger).await;
    Ok(())
}
