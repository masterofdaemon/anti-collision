use std::collections::VecDeque;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::time::{interval, sleep};
use futures_util::StreamExt;
use tokio::sync::watch;
use std::sync::Arc;

#[derive(Clone)]
pub struct Config {
    pub threshold_mbps: f64,
    pub sleep_duration: Duration,
    pub check_duration: Duration,
    pub streams: usize,
    pub rolling_window_secs: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            threshold_mbps: 20.0,
            sleep_duration: Duration::from_secs(3600),
            check_duration: Duration::from_secs(5),
            streams: 4,
            rolling_window_secs: 5,
        }
    }
}

pub struct CycleResult {
    pub avg_mbps: f64,
}

pub const DEFAULT_TEST_URL: &str = "https://download.thinkbroadband.com/100MB.zip";

async fn run_saturation_cycle(
    url: &str,
    duration: Option<Duration>,
    client: &reqwest::Client,
    config: &Config,
    stop_rx: &mut watch::Receiver<bool>,
    log: Arc<dyn Fn(&str) + Send + Sync>,
) -> CycleResult {
    let (tx, mut rx) = mpsc::channel(1024);
    let mut workers = Vec::new();

    // Spawn workers
    for i in 0..config.streams {
        let tx = tx.clone();
        let client = client.clone();
        let url = url.to_string();
        let log = log.clone();
        workers.push(tokio::spawn(async move {
            let mut last_error_log = Instant::now() - Duration::from_secs(60);
            loop {
                match client.get(&url).send().await {
                    Ok(resp) => {
                        if !resp.status().is_success() {
                            if i == 0 && last_error_log.elapsed() >= Duration::from_secs(5) {
                                let msg = format!(
                                    "  WARN: target responded with HTTP {}",
                                    resp.status()
                                );
                                log(&msg);
                                last_error_log = Instant::now();
                            }
                            sleep(Duration::from_secs(2)).await;
                            continue;
                        }
                        let mut stream = resp.bytes_stream();
                        while let Some(chunk) = stream.next().await {
                            match chunk {
                                Ok(bytes) => {
                                    if tx.send(bytes.len()).await.is_err() {
                                        return;
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                    }
                    Err(err) => {
                        if i == 0 && last_error_log.elapsed() >= Duration::from_secs(5) {
                            let msg = format!("  WARN: request failed: {err}");
                            log(&msg);
                            last_error_log = Instant::now();
                        }
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
    let mut samples: VecDeque<f64> = VecDeque::with_capacity(config.rolling_window_secs);
    let mut zero_streak = Duration::from_secs(0);
    let mut warned_zero = false;

    loop {
        tokio::select! {
            Some(len) = rx.recv() => {
                let len = len as u64;
                window_bytes += len;
                total_bytes += len;
            }
            _ = tick.tick() => {
                if *stop_rx.borrow() {
                    for w in workers { w.abort(); }
                    return CycleResult { avg_mbps: 0.0 };
                }

                let elapsed = last_report.elapsed().as_secs_f64();
                if elapsed > 0.0 {
                    let last_mbps = (window_bytes as f64 * 8.0) / (1_000_000.0 * elapsed);
                    log(&format!("  Current: {:.2} Mbps", last_mbps));

                    if window_bytes == 0 {
                        zero_streak += Duration::from_secs_f64(elapsed);
                        if !warned_zero && zero_streak >= Duration::from_secs(3) {
                            log("  WARN: no data received yet; target may be blocked or down.");
                            warned_zero = true;
                        }
                    } else {
                        zero_streak = Duration::from_secs(0);
                        warned_zero = false;
                    }

                    samples.push_back(last_mbps);
                    if samples.len() > config.rolling_window_secs {
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
                            return CycleResult { avg_mbps };
                        }
                    }

                    // If we are in "continuous" (Saturate) mode, return when speed recovers
                    if duration.is_none() {
                        if rolling_avg >= config.threshold_mbps {
                            good_elapsed += Duration::from_secs_f64(elapsed);
                        } else {
                            good_elapsed = Duration::from_secs(0);
                        }
                        if good_elapsed >= Duration::from_secs(15) {
                            log(&format!("  Speed restored to {:.2} Mbps.", last_mbps));
                            for w in workers { w.abort(); }
                            return CycleResult { avg_mbps: last_mbps };
                        }
                    }
                }
                window_bytes = 0;
                last_report = Instant::now();
            }
            _ = stop_rx.changed() => {
                if *stop_rx.borrow() {
                    for w in workers { w.abort(); }
                    return CycleResult { avg_mbps: 0.0 };
                }
            }
        }
    }
}

pub async fn run_monitor_loop(
    url: &str,
    client: &reqwest::Client,
    config: &Config,
    mut stop_rx: watch::Receiver<bool>,
    log: Arc<dyn Fn(&str) + Send + Sync>,
) {
    log("=== Automated Anti-Collision Saturator ===");
    log(&format!("Threshold: {} Mbps", config.threshold_mbps));
    log(&format!("Target:    {}", url));
    log("Execution: Monitoring -> [Saturation] -> 1h Sleep");
    log("------------------------------------------");

    loop {
        if *stop_rx.borrow() {
            break;
        }
        log(&format!(
            "CHECKING: Measuring current speed ({}s sample)...",
            config.check_duration.as_secs()
        ));
        let result = run_saturation_cycle(
            url,
            Some(config.check_duration),
            client,
            config,
            &mut stop_rx,
            log.clone(),
        )
        .await;

        if *stop_rx.borrow() {
            break;
        }

        if result.avg_mbps >= config.threshold_mbps {
            log(&format!(
                "OK: Speed is {:.2} Mbps. Sleeping for 1 hour...",
                result.avg_mbps
            ));
            tokio::select! {
                _ = sleep(config.sleep_duration) => {}
                _ = stop_rx.changed() => {
                    if *stop_rx.borrow() { break; }
                }
            }
        } else {
            log(&format!(
                "POOR: Speed is {:.2} Mbps. Starting continuous saturation...",
                result.avg_mbps
            ));
            run_saturation_cycle(url, None, client, config, &mut stop_rx, log.clone()).await;
            if *stop_rx.borrow() {
                break;
            }
            log("RECOVERED: Speed back to normal. Entering 1-hour sleep cycle.");
            tokio::select! {
                _ = sleep(config.sleep_duration) => {}
                _ = stop_rx.changed() => {
                    if *stop_rx.borrow() { break; }
                }
            }
        }
    }
}
