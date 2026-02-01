use std::time::{Duration, Instant};
use std::collections::VecDeque;
use tokio::time::{sleep, interval};
use tokio::sync::mpsc;
use std::env;
use futures_util::StreamExt;

const THRESHOLD_MBPS: f64 = 20.0;
const SLEEP_DURATION: Duration = Duration::from_secs(3600); // 1 hour
const CHECK_DURATION: Duration = Duration::from_secs(5);    // 5 seconds check
const STREAMS: usize = 4;
const ROLLING_WINDOW_SECS: usize = 5;

async fn run_saturation_cycle(url: &str, duration: Option<Duration>, client: &reqwest::Client) -> f64 {
    let (tx, mut rx) = mpsc::channel(1024);
    let mut workers = Vec::new();

    // Spawn workers
    for _i in 0..STREAMS {
        let tx = tx.clone();
        let client = client.clone();
        let url = url.to_string();
        workers.push(tokio::spawn(async move {
            loop {
                match client.get(&url).send().await {
                    Ok(resp) => {
                        if !resp.status().is_success() {
                            sleep(Duration::from_secs(2)).await;
                            continue;
                        }
                        let mut stream = resp.bytes_stream();
                        while let Some(chunk) = stream.next().await {
                            match chunk {
                                Ok(bytes) => {
                                    if tx.send(bytes.len()).await.is_err() { return; }
                                }
                                Err(_) => break,
                            }
                        }
                    }
                    Err(_) => {
                        sleep(Duration::from_secs(1)).await;
                    }
                }
            }
        }));
    }

    let start = Instant::now();
    let mut window_bytes = 0u64;
    let mut total_bytes = 0u64;
    let mut last_report = Instant::now();
    let mut tick = interval(Duration::from_secs(1));
    let mut good_elapsed = Duration::from_secs(0);
    let mut samples: VecDeque<f64> = VecDeque::with_capacity(ROLLING_WINDOW_SECS);

    loop {
        tokio::select! {
            Some(len) = rx.recv() => {
                let len = len as u64;
                window_bytes += len;
                total_bytes += len;
            }
            _ = tick.tick() => {
                let elapsed = last_report.elapsed().as_secs_f64();
                if elapsed > 0.0 {
                    let last_mbps = (window_bytes as f64 * 8.0) / (1_000_000.0 * elapsed);
                    println!("  Current: {:.2} Mbps", last_mbps);
                    samples.push_back(last_mbps);
                    if samples.len() > ROLLING_WINDOW_SECS {
                        samples.pop_front();
                    }
                    let rolling_avg = if samples.is_empty() {
                        last_mbps
                    } else {
                        samples.iter().sum::<f64>() / samples.len() as f64
                    };
                    
                    // If we have a duration goal (Check mode), handle completion
                    if let Some(d) = duration {
                        if start.elapsed() >= d {
                             for w in workers { w.abort(); }
                             let total_elapsed = start.elapsed().as_secs_f64().max(0.000_001);
                             let avg_mbps = (total_bytes as f64 * 8.0) / (1_000_000.0 * total_elapsed);
                             return avg_mbps;
                        }
                    }
                    
                    // If we are in "continuous" (Saturate) mode, return when speed recovers
                    if duration.is_none() {
                        if rolling_avg >= THRESHOLD_MBPS {
                            good_elapsed += Duration::from_secs_f64(elapsed);
                        } else {
                            good_elapsed = Duration::from_secs(0);
                        }
                        if good_elapsed >= Duration::from_secs(15) {
                            println!("  Speed restored to {:.2} Mbps.", last_mbps);
                            for w in workers { w.abort(); }
                            return last_mbps;
                        }
                    }
                }
                window_bytes = 0;
                last_report = Instant::now();
            }
        }
    }
}

use chrono::Local;

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

    println!("=== Automated Anti-Collision Saturator ===");
    println!("Threshold: {} Mbps", THRESHOLD_MBPS);
    println!("Target:    {}", url);
    println!("Execution: Monitoring -> [Saturation] -> 1h Sleep");
    println!("------------------------------------------");
    
    loop {
        log(&format!("CHECKING: Measuring current speed ({}s sample)...", CHECK_DURATION.as_secs()));
        let speed = run_saturation_cycle(url, Some(CHECK_DURATION), &client).await;
        
        if speed >= THRESHOLD_MBPS {
            log(&format!("OK: Speed is {:.2} Mbps. Sleeping for 1 hour...", speed));
            sleep(SLEEP_DURATION).await;
        } else {
            log(&format!("POOR: Speed is {:.2} Mbps. Starting continuous saturation...", speed));
            run_saturation_cycle(url, None, &client).await;
            log("RECOVERED: Speed back to normal. Entering 1-hour sleep cycle.");
            sleep(SLEEP_DURATION).await;
        }
    }
}
