use std::collections::VecDeque;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::time::{interval, sleep, timeout};
use futures_util::{StreamExt, stream::FuturesUnordered};
use tokio::sync::watch;
use std::sync::Arc;
use reqwest::header::RANGE;

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

pub const DEFAULT_TEST_URL: &str = "https://speedtest.selectel.ru/100MB";
pub const DEFAULT_TEST_URLS: &[&str] = &[
    "https://speedtest.selectel.ru/100MB",
    "https://fsn1-speed.hetzner.com/100MB.bin",
    "https://hel1-speed.hetzner.com/100MB.bin",
    "https://ash-speed.hetzner.com/100MB.bin",
    "https://hil-speed.hetzner.com/100MB.bin",
    "https://speed.af.de/files/100MB.bin",
    "https://us.speedtest.hostiserver.com/100MB",
    "https://eu.speedtest.hostiserver.com/100MB",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionMode {
    Latency,
    Throughput,
}

impl SelectionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            SelectionMode::Latency => "latency",
            SelectionMode::Throughput => "throughput",
        }
    }

    pub fn from_str(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "throughput" | "пропускная способность" | "скорость" => SelectionMode::Throughput,
            _ => SelectionMode::Latency,
        }
    }
}

pub fn build_candidate_urls(preferred: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(url) = preferred {
        let trimmed = url.trim();
        if !trimmed.is_empty() {
            out.push(trimmed.to_string());
        }
    }
    for &url in DEFAULT_TEST_URLS {
        if !out.iter().any(|u| u == url) {
            out.push(url.to_string());
        }
    }
    out
}

fn log_opt(log: &Option<Arc<dyn Fn(&str) + Send + Sync>>, msg: &str) {
    if let Some(log) = log {
        log(msg);
    }
}

async fn probe_latency(url: &str, client: &reqwest::Client) -> Result<Duration, String> {
    let start = Instant::now();
    let req = client.get(url).header(RANGE, "bytes=0-0");
    let resp = timeout(Duration::from_secs(5), req.send())
        .await
        .map_err(|_| "тайм-аут".to_string())?
        .map_err(|err| err.to_string())?;

    if !resp.status().is_success() {
        return Err(format!("код HTTP {}", resp.status()));
    }

    let mut stream = resp.bytes_stream();
    let first = timeout(Duration::from_secs(5), stream.next())
        .await
        .map_err(|_| "тайм-аут".to_string())?;
    match first {
        Some(Ok(bytes)) if !bytes.is_empty() => Ok(start.elapsed()),
        Some(Ok(_)) => Ok(start.elapsed()),
        Some(Err(err)) => Err(err.to_string()),
        None => Err("нет данных".to_string()),
    }
}

async fn select_best_latency(
    urls: &[String],
    client: &reqwest::Client,
    log: Option<Arc<dyn Fn(&str) + Send + Sync>>,
) -> Option<(String, Duration)> {
    let mut tasks = FuturesUnordered::new();
    for url in urls {
        let url = url.clone();
        let client = client.clone();
        tasks.push(async move { (url.clone(), probe_latency(&url, &client).await) });
    }

    let mut best: Option<(String, Duration)> = None;
    while let Some((url, result)) = tasks.next().await {
        match result {
            Ok(latency) => {
                log_opt(&log, &format!("  ВЫБОР: {url} доступен ({:.0} мс)", latency.as_secs_f64() * 1000.0));
                let replace = match best {
                    None => true,
                    Some((_, best_latency)) => latency < best_latency,
                };
                if replace {
                    best = Some((url, latency));
                }
            }
            Err(err) => {
                log_opt(&log, &format!("  ПРЕДУПРЕЖДЕНИЕ: проверка не удалась для {url}: {err}"));
            }
        }
    }

    best
}

async fn probe_throughput(
    url: &str,
    client: &reqwest::Client,
    streams: usize,
) -> Result<f64, String> {
    let probe_streams = streams.max(1).min(16);
    let config = Config {
        streams: probe_streams,
        ..Config::default()
    };
    let (_stop_tx, mut stop_rx) = watch::channel(false);
    let silent_log: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|_| {});
    let duration = Duration::from_secs(3);

    let result = run_saturation_cycle(url, Some(duration), client, &config, &mut stop_rx, silent_log).await;
    Ok(result.avg_mbps)
}

async fn select_best_throughput(
    urls: &[String],
    client: &reqwest::Client,
    log: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    streams: usize,
) -> Option<(String, f64)> {
    let mut best: Option<(String, f64)> = None;
    for url in urls {
        log_opt(&log, &format!("  ВЫБОР: проверяем пропускную способность {url} (3 c)..."));
        match probe_throughput(url, client, streams).await {
            Ok(mbps) => {
                log_opt(&log, &format!("  ВЫБОР: {url} доступен ({:.2} Мбит/с)", mbps));
                let replace = match best {
                    None => true,
                    Some((_, best_mbps)) => mbps > best_mbps,
                };
                if replace {
                    best = Some((url.clone(), mbps));
                }
            }
            Err(err) => {
                log_opt(&log, &format!("  ПРЕДУПРЕЖДЕНИЕ: проверка пропускной способности не удалась для {url}: {err}"));
            }
        }
    }

    best
}

pub async fn select_available_url(
    candidates: &[String],
    prefer_first: bool,
    mode: SelectionMode,
    probe_streams: usize,
    client: &reqwest::Client,
    log: Option<Arc<dyn Fn(&str) + Send + Sync>>,
) -> String {
    if candidates.is_empty() {
        return DEFAULT_TEST_URL.to_string();
    }

    if prefer_first {
        let first = &candidates[0];
        log_opt(&log, &format!("ВЫБОР: проверяем предпочтительный адрес {first}..."));
        match probe_latency(first, client).await {
            Ok(latency) => {
                log_opt(&log, &format!("ВЫБОР: используем {first} ({:.0} мс)", latency.as_secs_f64() * 1000.0));
                return first.clone();
            }
            Err(err) => {
                log_opt(&log, &format!("  ПРЕДУПРЕЖДЕНИЕ: предпочтительный адрес недоступен: {err}"));
            }
        }
    }

    let pool = if prefer_first && candidates.len() > 1 {
        &candidates[1..]
    } else {
        candidates
    };

    if !pool.is_empty() {
        match mode {
            SelectionMode::Latency => {
                log_opt(&log, &format!("ВЫБОР: проверяем {} адресов с минимальной задержкой...", pool.len()));
                if let Some((url, latency)) = select_best_latency(pool, client, log.clone()).await {
                    log_opt(&log, &format!("ВЫБОР: используем {url} ({:.0} мс)", latency.as_secs_f64() * 1000.0));
                    return url;
                }
            }
            SelectionMode::Throughput => {
                log_opt(&log, &format!("ВЫБОР: проверяем {} адресов с максимальной скоростью...", pool.len()));
                if let Some((url, mbps)) = select_best_throughput(pool, client, log.clone(), probe_streams).await {
                    log_opt(&log, &format!("ВЫБОР: используем {url} ({:.2} Мбит/с)", mbps));
                    return url;
                }
            }
        }
    }

    log_opt(&log, "ПРЕДУПРЕЖДЕНИЕ: ни один адрес не доступен, используем первый из списка.");
    candidates[0].clone()
}

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
                                    "  ПРЕДУПРЕЖДЕНИЕ: сервер вернул HTTP {}",
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
                            let msg = format!("  ПРЕДУПРЕЖДЕНИЕ: запрос завершился ошибкой: {err}");
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
                    log(&format!("  Текущая скорость: {:.2} Мбит/с", last_mbps));

                    if window_bytes == 0 {
                        zero_streak += Duration::from_secs_f64(elapsed);
                        if !warned_zero && zero_streak >= Duration::from_secs(3) {
                            log("  ПРЕДУПРЕЖДЕНИЕ: пока нет данных; адрес может быть заблокирован или недоступен.");
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
                            log(&format!("  Скорость восстановилась до {:.2} Мбит/с.", last_mbps));
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
    log("=== Автоматический насыщатор Anti-Collision ===");
    log(&format!("Порог: {} Мбит/с", config.threshold_mbps));
    log(&format!("Потоки: {}", config.streams));
    log(&format!("Адрес: {}", url));
    log("Режим: Мониторинг -> [Насыщение] -> Сон 1 час");
    log("------------------------------------------");

    loop {
        if *stop_rx.borrow() {
            break;
        }
        log(&format!(
            "ПРОВЕРКА: измеряем текущую скорость (окно {} c)...",
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
                "НОРМА: скорость {:.2} Мбит/с. Уходим в сон на 1 час...",
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
                "НИЗКАЯ СКОРОСТЬ: {:.2} Мбит/с. Запускаем непрерывное насыщение...",
                result.avg_mbps
            ));
            run_saturation_cycle(url, None, client, config, &mut stop_rx, log.clone()).await;
            if *stop_rx.borrow() {
                break;
            }
            log("ВОССТАНОВЛЕНО: скорость вернулась к норме. Переходим в цикл сна на 1 час.");
            tokio::select! {
                _ = sleep(config.sleep_duration) => {}
                _ = stop_rx.changed() => {
                    if *stop_rx.borrow() { break; }
                }
            }
        }
    }
}
