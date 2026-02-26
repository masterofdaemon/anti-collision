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
            threshold_mbps: 60.0,
            sleep_duration: Duration::from_secs(3600),
            check_duration: Duration::from_secs(5),
            streams: 8,
            rolling_window_secs: 5,
        }
    }
}

pub struct CycleResult {
    pub avg_mbps: f64,
}

pub struct SelectionResult {
    pub selected: String,
    pub rotation_targets: Vec<String>,
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
) -> Vec<(String, Duration)> {
    let mut tasks = FuturesUnordered::new();
    for url in urls {
        let url = url.clone();
        let client = client.clone();
        tasks.push(async move { (url.clone(), probe_latency(&url, &client).await) });
    }

    let mut successful: Vec<(String, Duration)> = Vec::new();
    while let Some((url, result)) = tasks.next().await {
        match result {
            Ok(latency) => {
                log_opt(&log, &format!("  ВЫБОР: {url} доступен ({:.0} мс)", latency.as_secs_f64() * 1000.0));
                successful.push((url, latency));
            }
            Err(err) => {
                log_opt(&log, &format!("  ПРЕДУПРЕЖДЕНИЕ: проверка не удалась для {url}: {err}"));
            }
        }
    }

    successful.sort_by(|a, b| a.1.cmp(&b.1));
    successful
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
    let targets = vec![url.to_string()];
    let result = run_saturation_cycle(&targets, Some(duration), client, &config, &mut stop_rx, silent_log).await;
    Ok(result.avg_mbps)
}

async fn select_best_throughput(
    urls: &[String],
    client: &reqwest::Client,
    log: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    streams: usize,
) -> Vec<(String, f64)> {
    let mut successful: Vec<(String, f64)> = Vec::new();
    for url in urls {
        log_opt(&log, &format!("  ВЫБОР: проверяем пропускную способность {url} (3 c)..."));
        match probe_throughput(url, client, streams).await {
            Ok(mbps) => {
                log_opt(&log, &format!("  ВЫБОР: {url} доступен ({:.2} Мбит/с)", mbps));
                successful.push((url.clone(), mbps));
            }
            Err(err) => {
                log_opt(&log, &format!("  ПРЕДУПРЕЖДЕНИЕ: проверка пропускной способности не удалась для {url}: {err}"));
            }
        }
    }

    successful.sort_by(|a, b| b.1.total_cmp(&a.1));
    successful
}

pub async fn select_available_url(
    candidates: &[String],
    prefer_first: bool,
    mode: SelectionMode,
    probe_streams: usize,
    client: &reqwest::Client,
    log: Option<Arc<dyn Fn(&str) + Send + Sync>>,
) -> SelectionResult {
    if candidates.is_empty() {
        let fallback = DEFAULT_TEST_URL.to_string();
        return SelectionResult {
            selected: fallback.clone(),
            rotation_targets: vec![fallback],
        };
    }

    let mut selected: Option<String> = None;
    let mut rotation_targets: Vec<String> = Vec::new();

    if prefer_first {
        let first = &candidates[0];
        log_opt(&log, &format!("ВЫБОР: проверяем предпочтительный адрес {first}..."));
        match probe_latency(first, client).await {
            Ok(latency) => {
                log_opt(&log, &format!("ВЫБОР: используем {first} ({:.0} мс)", latency.as_secs_f64() * 1000.0));
                selected = Some(first.clone());
                rotation_targets.push(first.clone());
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
                let ranked = select_best_latency(pool, client, log.clone()).await;
                if selected.is_none() {
                    if let Some((url, latency)) = ranked.first() {
                        let url = url.clone();
                        let latency = *latency;
                        log_opt(&log, &format!("ВЫБОР: используем {url} ({:.0} мс)", latency.as_secs_f64() * 1000.0));
                        selected = Some(url);
                    }
                }
                for (url, _) in ranked {
                    if !rotation_targets.iter().any(|u| u == &url) {
                        rotation_targets.push(url);
                    }
                }
            }
            SelectionMode::Throughput => {
                log_opt(&log, &format!("ВЫБОР: проверяем {} адресов с максимальной скоростью...", pool.len()));
                let ranked = select_best_throughput(pool, client, log.clone(), probe_streams).await;
                if selected.is_none() {
                    if let Some((url, mbps)) = ranked.first() {
                        let url = url.clone();
                        let mbps = *mbps;
                        log_opt(&log, &format!("ВЫБОР: используем {url} ({:.2} Мбит/с)", mbps));
                        selected = Some(url);
                    }
                }
                for (url, _) in ranked {
                    if !rotation_targets.iter().any(|u| u == &url) {
                        rotation_targets.push(url);
                    }
                }
            }
        }
    }

    if selected.is_none() {
        log_opt(&log, "ПРЕДУПРЕЖДЕНИЕ: ни один адрес не доступен, используем первый из списка.");
        selected = Some(candidates[0].clone());
    }
    let selected = selected.unwrap_or_else(|| candidates[0].clone());
    if !rotation_targets.iter().any(|u| u == &selected) {
        rotation_targets.insert(0, selected.clone());
    }
    if rotation_targets.is_empty() {
        rotation_targets.push(selected.clone());
    }
    if rotation_targets.len() > 1 {
        log_opt(&log, &format!("ВЫБОР: ротация активна, источников: {}", rotation_targets.len()));
    } else {
        log_opt(&log, "ВЫБОР: доступен только один источник, ротация ограничена.");
    }

    SelectionResult {
        selected,
        rotation_targets,
    }
}

fn with_cache_buster(url: &str, worker: usize, seq: u64) -> String {
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}ac_worker={worker}&ac_seq={seq}")
}

async fn run_saturation_cycle(
    targets: &[String],
    duration: Option<Duration>,
    client: &reqwest::Client,
    config: &Config,
    stop_rx: &mut watch::Receiver<bool>,
    log: Arc<dyn Fn(&str) + Send + Sync>,
) -> CycleResult {
    let (tx, mut rx) = mpsc::channel(1024);
    let mut workers = Vec::new();
    let targets = if targets.is_empty() {
        vec![DEFAULT_TEST_URL.to_string()]
    } else {
        targets.to_vec()
    };
    if targets.len() > 1 {
        log(&format!("  РОТАЦИЯ: используем {} источников загрузки.", targets.len()));
    }
    let targets = Arc::new(targets);

    // Spawn workers
    for i in 0..config.streams {
        let tx = tx.clone();
        let client = client.clone();
        let targets = targets.clone();
        let log = log.clone();
        workers.push(tokio::spawn(async move {
            let mut last_error_log = Instant::now() - Duration::from_secs(60);
            let mut target_idx = i % targets.len();
            let mut request_seq: u64 = 0;
            loop {
                let base_url = targets[target_idx].clone();
                target_idx = (target_idx + 1) % targets.len();
                let request_url = with_cache_buster(&base_url, i, request_seq);
                request_seq = request_seq.wrapping_add(1);

                match client.get(&request_url).send().await {
                    Ok(resp) => {
                        if !resp.status().is_success() {
                            if i == 0 && last_error_log.elapsed() >= Duration::from_secs(5) {
                                let msg = format!(
                                    "  ПРЕДУПРЕЖДЕНИЕ: сервер {base_url} вернул HTTP {}",
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
                            let msg = format!("  ПРЕДУПРЕЖДЕНИЕ: запрос к {base_url} завершился ошибкой: {err}");
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
    targets: &[String],
    client: &reqwest::Client,
    config: &Config,
    mut stop_rx: watch::Receiver<bool>,
    log: Arc<dyn Fn(&str) + Send + Sync>,
) {
    let primary_target = targets
        .first()
        .cloned()
        .unwrap_or_else(|| DEFAULT_TEST_URL.to_string());
    let check_targets = vec![primary_target.clone()];
    let saturation_targets = if targets.is_empty() {
        vec![primary_target.clone()]
    } else {
        targets.to_vec()
    };

    log("=== Автоматический насыщатор Anti-Collision ===");
    log(&format!("Порог: {} Мбит/с", config.threshold_mbps));
    log(&format!("Потоки: {}", config.streams));
    log(&format!("Адрес (основной): {}", primary_target));
    if saturation_targets.len() > 1 {
        log(&format!("Ротация источников: {} адресов", saturation_targets.len()));
    }
    log("Режим: Мониторинг -> [Насыщение] -> Сон 1 час");
    if cfg!(target_os = "android") {
        log("ANDROID: в фоне система может выгрузить приложение. Если через час нет новой проверки — откройте приложение и нажмите «Запустить».");
    }
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
            &check_targets,
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
            if cfg!(target_os = "android") {
                log("ANDROID: напоминание — через 1 час начнется новая проверка, только если приложение не выгружено из фона.");
            }
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
            run_saturation_cycle(&saturation_targets, None, client, config, &mut stop_rx, log.clone()).await;
            if *stop_rx.borrow() {
                break;
            }
            log("ВОССТАНОВЛЕНО: скорость вернулась к норме. Переходим в цикл сна на 1 час.");
            if cfg!(target_os = "android") {
                log("ANDROID: напоминание — если система остановит приложение в фоне, откройте его и нажмите «Запустить».");
            }
            tokio::select! {
                _ = sleep(config.sleep_duration) => {}
                _ = stop_rx.changed() => {
                    if *stop_rx.borrow() { break; }
                }
            }
        }
    }
}
