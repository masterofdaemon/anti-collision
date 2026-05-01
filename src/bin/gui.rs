use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use chrono::Local;
use dioxus::prelude::*;
use dioxus::launch;
use tokio::runtime::Runtime;
use tokio::sync::watch;

use anti_collision::core::{
    build_candidate_urls,
    run_monitor_loop,
    select_available_url,
    Config,
    DEFAULT_TEST_URLS,
    SelectionMode,
};

const APP_CSS: &str = r#"
:root {
    --font-main: "Avenir Next", "SF Pro Display", "Segoe UI Variable", "Noto Sans", sans-serif;
    --font-mono: "JetBrains Mono", "SF Mono", "Consolas", monospace;
    --bg-top: #e2f2ff;
    --bg-bottom: #f6fbff;
    --surface: rgba(255, 255, 255, 0.86);
    --surface-solid: #f9fcff;
    --surface-strong: #ffffff;
    --border: #cbe0f5;
    --text: #0a2239;
    --subtext: #486987;
    --primary: #0f6fc6;
    --primary-strong: #0a5ca5;
    --danger: #7e8fa3;
    --danger-strong: #6a7d93;
    --success-bg: #dbf4ff;
    --success-border: #73c9f8;
    --success-text: #084f80;
    --idle-bg: #f0f5fa;
    --idle-border: #bfd0e0;
    --idle-text: #33526f;
    --warn-bg: #fff4e6;
    --warn-border: #ffd8a8;
    --warn-text: #8a4b08;
    --radius-lg: 18px;
    --radius-md: 12px;
    --radius-sm: 10px;
    --shadow-soft: 0 10px 24px rgba(14, 42, 66, 0.1);
    --shadow-hover: 0 14px 28px rgba(14, 42, 66, 0.14);
}

* {
    box-sizing: border-box;
}

html,
body {
    margin: 0;
    min-height: 100%;
}

body {
    font-family: var(--font-main);
    color: var(--text);
    background: linear-gradient(165deg, var(--bg-top) 0%, var(--bg-bottom) 62%);
    line-height: 1.35;
}

body::before,
body::after {
    content: "";
    position: fixed;
    inset: auto;
    border-radius: 999px;
    pointer-events: none;
    z-index: 0;
    filter: blur(2px);
}

body::before {
    width: 34vmax;
    height: 34vmax;
    top: -14vmax;
    right: -12vmax;
    background: radial-gradient(circle at center, rgba(72, 185, 247, 0.3), rgba(72, 185, 247, 0));
}

body::after {
    width: 30vmax;
    height: 30vmax;
    bottom: -12vmax;
    left: -10vmax;
    background: radial-gradient(circle at center, rgba(15, 111, 198, 0.2), rgba(15, 111, 198, 0));
}

.app-shell {
    position: relative;
    z-index: 1;
    width: min(1180px, 100%);
    margin: 0 auto;
    padding: 24px clamp(16px, 3.2vw, 36px) 30px;
    min-height: 100vh;
    display: grid;
    gap: 16px;
}

.hero {
    background: linear-gradient(130deg, rgba(255, 255, 255, 0.92) 0%, rgba(236, 247, 255, 0.84) 100%);
    border: 1px solid rgba(203, 224, 245, 0.95);
    border-radius: var(--radius-lg);
    padding: clamp(18px, 2.2vw, 26px);
    box-shadow: var(--shadow-soft);
    animation: rise-in 520ms cubic-bezier(0.22, 1, 0.36, 1) both;
}

.title {
    margin: 0;
    font-size: clamp(1.45rem, 1.1rem + 1.2vw, 2.15rem);
    letter-spacing: 0.01em;
}

.subtitle {
    margin: 8px 0 0;
    color: var(--subtext);
    font-size: clamp(0.95rem, 0.86rem + 0.25vw, 1.05rem);
}

.android-warning {
    margin: 0;
    padding: 11px 14px;
    border-radius: var(--radius-md);
    border: 1px solid var(--warn-border);
    background: var(--warn-bg);
    color: var(--warn-text);
    box-shadow: 0 6px 16px rgba(138, 75, 8, 0.08);
    animation: rise-in 520ms cubic-bezier(0.22, 1, 0.36, 1) both;
    animation-delay: 80ms;
}

.main-grid {
    display: grid;
    grid-template-columns: minmax(320px, 432px) minmax(0, 1fr);
    gap: 16px;
    align-items: start;
}

.card {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-soft);
    backdrop-filter: blur(8px);
    transition: box-shadow 180ms ease, transform 180ms ease;
}

.card:hover {
    transform: translateY(-1px);
    box-shadow: var(--shadow-hover);
}

.controls-card {
    padding: 18px;
    animation: rise-in 560ms cubic-bezier(0.22, 1, 0.36, 1) both;
    animation-delay: 70ms;
}

.section-title {
    margin: 0 0 4px;
    font-size: 1.03rem;
    font-weight: 650;
}

.section-hint {
    margin: 0 0 14px;
    color: var(--subtext);
    font-size: 0.92rem;
}

.form-grid {
    display: grid;
    gap: 12px;
    margin-bottom: 14px;
}

.field {
    display: grid;
    grid-template-columns: 132px minmax(0, 1fr);
    gap: 10px;
    align-items: center;
}

.field-label {
    font-size: 0.92rem;
    font-weight: 620;
    color: #173958;
}

.control {
    width: 100%;
    min-height: 44px;
    border-radius: var(--radius-sm);
    border: 1px solid var(--border);
    background: var(--surface-strong);
    color: var(--text);
    padding: 10px 12px;
    font-size: 0.96rem;
    transition: border-color 130ms ease, box-shadow 130ms ease, transform 130ms ease;
}

.control:hover {
    border-color: #9bc6eb;
}

.control:focus-visible {
    outline: 0;
    border-color: #5ea8e2;
    box-shadow: 0 0 0 3px rgba(94, 168, 226, 0.25);
}

.action-row {
    display: flex;
    gap: 10px;
    align-items: center;
    flex-wrap: wrap;
}

.btn {
    min-height: 44px;
    border: 1px solid transparent;
    border-radius: 11px;
    padding: 10px 16px;
    font-size: 0.95rem;
    font-weight: 620;
    cursor: pointer;
    transition: transform 130ms ease, box-shadow 160ms ease, background-color 140ms ease, border-color 140ms ease;
}

.btn:focus-visible {
    outline: 0;
    box-shadow: 0 0 0 3px rgba(76, 167, 235, 0.33);
}

.btn:active:not(:disabled) {
    transform: translateY(1px);
}

.btn-primary {
    color: #fff;
    background: linear-gradient(130deg, var(--primary) 0%, #1198da 100%);
    border-color: rgba(9, 74, 137, 0.32);
}

.btn-primary:hover:not(:disabled) {
    box-shadow: 0 8px 18px rgba(15, 111, 198, 0.32);
}

.btn-secondary {
    color: #213f5d;
    border-color: #b9cfe3;
    background: linear-gradient(140deg, #f7fbff 0%, #edf4fb 100%);
}

.btn-secondary:hover:not(:disabled) {
    border-color: #9fbad3;
    box-shadow: 0 8px 16px rgba(49, 83, 117, 0.15);
}

.btn:disabled {
    cursor: not-allowed;
    opacity: 0.52;
    transform: none;
    box-shadow: none;
}

.status-pill {
    margin-left: auto;
    min-height: 44px;
    display: inline-flex;
    align-items: center;
    gap: 7px;
    border-radius: 999px;
    padding: 8px 14px;
    font-size: 0.91rem;
    border: 1px solid;
    font-weight: 560;
}

.status-pill::before {
    content: "";
    width: 9px;
    height: 9px;
    border-radius: 999px;
    background: currentColor;
    opacity: 0.9;
}

.status-pill.is-running {
    background: var(--success-bg);
    border-color: var(--success-border);
    color: var(--success-text);
}

.status-pill.is-stopped {
    background: var(--idle-bg);
    border-color: var(--idle-border);
    color: var(--idle-text);
}

.logs-card {
    padding: 12px;
    display: grid;
    gap: 10px;
    animation: rise-in 560ms cubic-bezier(0.22, 1, 0.36, 1) both;
    animation-delay: 120ms;
}

.log-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 12px;
    padding: 2px 4px 0;
}

.log-title {
    margin: 0;
    font-size: 1rem;
}

.log-caption {
    margin: 0;
    font-size: 0.84rem;
    color: var(--subtext);
}

.log-panel {
    border: 1px solid #c2d8eb;
    border-radius: 14px;
    background: linear-gradient(145deg, #f7fbff 0%, #edf5fd 100%);
    padding: 12px;
    height: 420px;
    overflow: auto;
    scrollbar-width: thin;
}

.log-output {
    margin: 0;
    font-family: var(--font-mono);
    font-size: 12px;
    line-height: 1.45;
    color: #0f2b44;
    white-space: pre-wrap;
    word-break: break-word;
}

@keyframes rise-in {
    from {
        opacity: 0;
        transform: translateY(14px) scale(0.995);
    }
    to {
        opacity: 1;
        transform: translateY(0) scale(1);
    }
}

@media (max-width: 1100px) {
    .main-grid {
        grid-template-columns: minmax(0, 1fr);
    }
}

@media (max-width: 820px) {
    .app-shell {
        gap: 14px;
    }

    .controls-card {
        padding: 15px;
    }

    .field {
        grid-template-columns: minmax(0, 1fr);
        gap: 7px;
    }
}

@media (max-width: 560px) {
    .app-shell {
        padding: 14px 12px 18px;
    }

    .hero,
    .controls-card,
    .logs-card {
        border-radius: 14px;
    }

    .action-row {
        flex-direction: column;
        align-items: stretch;
    }

    .btn {
        width: 100%;
    }

    .status-pill {
        margin-left: 0;
        justify-content: center;
        width: 100%;
    }

    .log-panel {
        height: 45vh;
        min-height: 260px;
    }
}

@media (prefers-reduced-motion: reduce) {
    *,
    *::before,
    *::after {
        animation: none !important;
        transition-duration: 0ms !important;
        scroll-behavior: auto !important;
    }
}
"#;

#[derive(Clone)]
struct WorkerHandle {
    stop_tx: watch::Sender<bool>,
}

struct WorkerChannels {
    log_rx: Receiver<String>,
    worker: Option<WorkerHandle>,
}

fn spawn_worker(url: String, threshold_mbps: f64, streams: usize, mode: SelectionMode) -> WorkerChannels {
    let (log_tx, log_rx) = mpsc::channel::<String>();
    let (stop_tx, stop_rx) = watch::channel(false);

    thread::spawn(move || {
        let rt = match Runtime::new() {
            Ok(rt) => rt,
            Err(err) => {
                let _ = log_tx.send(format!("ОШИБКА: не удалось создать Tokio runtime: {err}"));
                return;
            }
        };

        rt.block_on(async move {
            let client = match reqwest::Client::builder()
                .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
                .tcp_keepalive(Duration::from_secs(60))
                .http1_only()
                .build()
            {
                Ok(client) => client,
                Err(err) => {
                    let _ = log_tx.send(format!("ОШИБКА: не удалось создать HTTP-клиент: {err}"));
                    return;
                }
            };

            let log: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |msg: &str| {
                let ts = Local::now().format("%Y-%m-%d %H:%M:%S");
                let _ = log_tx.send(format!("[{}] {}", ts, msg));
            });

            let config = Config {
                threshold_mbps,
                streams,
                ..Config::default()
            };

            let prefer_first = !url.trim().is_empty();
            let candidates = build_candidate_urls(Some(&url));
            let selection = select_available_url(&candidates, prefer_first, mode, streams, &client, Some(log.clone())).await;

            run_monitor_loop(&selection.rotation_targets, &client, &config, stop_rx, log).await;
        });
    });

    WorkerChannels {
        log_rx,
        worker: Some(WorkerHandle { stop_tx }),
    }
}

fn app() -> Element {
    let mut url = use_signal(|| String::new());
    let mut threshold = use_signal(|| Config::default().threshold_mbps);
    let mut streams = use_signal(|| Config::default().streams);
    let mut select_mode = use_signal(|| SelectionMode::Latency.as_str().to_string());
    let running = use_signal(|| false);
    let logs = use_signal(|| VecDeque::<String>::with_capacity(500));

    let worker: Signal<Arc<Mutex<WorkerChannels>>> = use_signal(|| {
        Arc::new(Mutex::new(WorkerChannels {
            log_rx: mpsc::channel::<String>().1,
            worker: None,
        }))
    });

    // Periodically drain logs from the worker into UI state.
    use_future(move || {
        let worker = worker.clone();
        let mut logs = logs.clone();
        async move {
            loop {
                {
                    let worker = worker();
                    let guard = worker.lock().unwrap();
                    while let Ok(line) = guard.log_rx.try_recv() {
                        logs.with_mut(|l| {
                            if l.len() >= 500 {
                                l.pop_front();
                            }
                            l.push_back(line);
                        });
                    }
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    });

    let on_start = {
        let url = url.clone();
        let threshold = threshold.clone();
        let mut running = running.clone();
        let worker = worker.clone();
        let mut logs = logs.clone();
        let streams = streams.clone();
        let select_mode = select_mode.clone();
        move |_| {
            if running() {
                return;
            }

            logs.with_mut(|l| {
                l.clear();
                l.push_back("[интерфейс] запуск...".to_string());
                if cfg!(target_os = "android") {
                    l.push_back("[интерфейс] android: если приложение выгружено из фона, откройте его и нажмите «Запустить».".to_string());
                }
            });

            let mode = SelectionMode::from_str(&select_mode());
            let chans = spawn_worker(url(), threshold(), streams(), mode);
            let worker = worker();
            *worker.lock().unwrap() = chans;
            running.set(true);
        }
    };

    let on_stop = {
        let mut running = running.clone();
        let worker = worker.clone();
        let mut logs = logs.clone();
        move |_| {
            {
                let worker = worker();
                let mut guard = worker.lock().unwrap();
                if let Some(handle) = guard.worker.take() {
                    let _ = handle.stop_tx.send(true);
                }
            }
            logs.with_mut(|l| {
                l.push_back("[интерфейс] запрошена остановка".to_string());
            });
            running.set(false);
        }
    };

    let status_text = if running() { "Работает" } else { "Остановлено" };
    let status_class = if running() {
        "status-pill is-running"
    } else {
        "status-pill is-stopped"
    };

    rsx! {
        div { class: "app-shell",
            style { "{APP_CSS}" }
            section { class: "hero",
                h1 { class: "title", "Насыщатор Anti-Collision" }
                p { class: "subtitle", "Настольный интерфейс Dioxus (без трея)" }
            }
            if cfg!(target_os = "android") {
                p {
                    class: "android-warning",
                    "Android: ОС может остановить приложение в фоне. Если через час новая проверка не началась, откройте приложение и нажмите «Запустить»."
                }
            }

            div { class: "main-grid",
                section { class: "card controls-card",
                    h2 { class: "section-title", "Параметры" }
                    p { class: "section-hint", "Управление источником и порогами восстановления канала." }

                    div { class: "form-grid",
                        div { class: "field",
                            label { class: "field-label", "Сервер" }
                            select {
                                class: "control",
                                value: "{url}",
                                oninput: move |evt| url.set(evt.value()),
                                option { value: "", "Авто (лучший доступный)" }
                                for entry in DEFAULT_TEST_URLS.iter() {
                                    option { value: "{entry}", "{entry}" }
                                }
                            }
                        }

                        div { class: "field",
                            label { class: "field-label", "Порог" }
                            input {
                                class: "control",
                                value: "{threshold}",
                                oninput: move |evt| {
                                    if let Ok(v) = evt.value().parse::<f64>() {
                                        threshold.set(v);
                                    }
                                },
                            }
                        }

                        div { class: "field",
                            label { class: "field-label", "Потоки" }
                            input {
                                class: "control",
                                value: "{streams}",
                                oninput: move |evt| {
                                    if let Ok(v) = evt.value().parse::<usize>() {
                                        let v = v.clamp(1, 64);
                                        streams.set(v);
                                    }
                                },
                            }
                        }

                        div { class: "field",
                            label { class: "field-label", "Режим выбора" }
                            select {
                                class: "control",
                                value: "{select_mode}",
                                oninput: move |evt| select_mode.set(evt.value()),
                                option { value: "latency", "По задержке (рекомендуется)" }
                                option { value: "throughput", "По скорости (самый быстрый)" }
                            }
                        }
                    }

                    div { class: "action-row",
                        button {
                            class: "btn btn-primary",
                            onclick: on_start,
                            disabled: running(),
                            "Запустить"
                        }
                        button {
                            class: "btn btn-secondary",
                            onclick: on_stop,
                            disabled: !running(),
                            "Остановить"
                        }
                        div { class: "{status_class}",
                            strong { "Статус:" }
                            span { "{status_text}" }
                        }
                    }
                }

                section { class: "card logs-card",
                    div { class: "log-head",
                        h2 { class: "log-title", "Журнал мониторинга" }
                        p { class: "log-caption", "Поток событий в реальном времени" }
                    }
                    div { class: "log-panel",
                        pre { class: "log-output",
                            for line in logs().iter() {
                                "{line}\n"
                            }
                        }
                    }
                }
            }
        }
    }
}

fn main() {
    launch(app);
}
