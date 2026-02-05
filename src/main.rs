use std::env;
use std::time::Duration;
use tokio::sync::watch;
use chrono::Local;

use anti_collision::core::{
    build_candidate_urls,
    run_monitor_loop,
    select_available_url,
    Config,
    DEFAULT_TEST_URL,
    SelectionMode,
};
use std::sync::Arc;

fn log(msg: &str) {
    println!("[{}] {}", Local::now().format("%Y-%m-%d %H:%M:%S"), msg);
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .tcp_keepalive(Duration::from_secs(60))
        .http1_only()
        .build()?;

    let config = Config::default();
    let (_stop_tx, stop_rx) = watch::channel(false);
    let logger: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(log);
    let preferred = args.get(1).map(|s| s.as_str());
    let candidates = build_candidate_urls(preferred.or(Some(DEFAULT_TEST_URL)));
    let selected = select_available_url(
        &candidates,
        preferred.is_some(),
        SelectionMode::Latency,
        Config::default().streams,
        &client,
        Some(logger.clone()),
    )
    .await;
    run_monitor_loop(&selected, &client, &config, stop_rx, logger).await;
    Ok(())
}
