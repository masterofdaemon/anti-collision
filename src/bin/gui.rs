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

use anti_collision::core::{run_monitor_loop, Config, DEFAULT_TEST_URL};

#[derive(Clone)]
struct WorkerHandle {
    stop_tx: watch::Sender<bool>,
}

struct WorkerChannels {
    log_rx: Receiver<String>,
    worker: Option<WorkerHandle>,
}

fn spawn_worker(url: String, threshold_mbps: f64) -> WorkerChannels {
    let (log_tx, log_rx) = mpsc::channel::<String>();
    let (stop_tx, stop_rx) = watch::channel(false);

    thread::spawn(move || {
        let rt = match Runtime::new() {
            Ok(rt) => rt,
            Err(err) => {
                let _ = log_tx.send(format!("ERR: Failed to create Tokio runtime: {err}"));
                return;
            }
        };

        rt.block_on(async move {
            let client = match reqwest::Client::builder()
                .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
                .tcp_keepalive(Duration::from_secs(60))
                .build()
            {
                Ok(client) => client,
                Err(err) => {
                    let _ = log_tx.send(format!("ERR: Failed to build HTTP client: {err}"));
                    return;
                }
            };

            let config = Config {
                threshold_mbps,
                ..Config::default()
            };

            let log: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |msg: &str| {
                let ts = Local::now().format("%Y-%m-%d %H:%M:%S");
                let _ = log_tx.send(format!("[{}] {}", ts, msg));
            });

            run_monitor_loop(&url, &client, &config, stop_rx, log).await;
        });
    });

    WorkerChannels {
        log_rx,
        worker: Some(WorkerHandle { stop_tx }),
    }
}

fn App() -> Element {
    let mut url = use_signal(|| DEFAULT_TEST_URL.to_string());
    let mut threshold = use_signal(|| 20.0_f64);
    let mut running = use_signal(|| false);
    let mut logs = use_signal(|| VecDeque::<String>::with_capacity(500));

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
        let mut url = url.clone();
        let mut threshold = threshold.clone();
        let mut running = running.clone();
        let worker = worker.clone();
        let mut logs = logs.clone();
        move |_| {
            if running() {
                return;
            }

            logs.with_mut(|l| {
                l.clear();
                l.push_back("[ui] starting...".to_string());
            });

            let chans = spawn_worker(url(), threshold());
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
                l.push_back("[ui] stop requested".to_string());
            });
            running.set(false);
        }
    };

    rsx! {
        div {
            style: "font-family: ui-sans-serif, system-ui; padding: 16px; max-width: 900px;",

            h1 { style: "margin: 0 0 8px 0;", "Anti-Collision Saturator" }
            p { style: "margin: 0 0 16px 0; color: #444;", "Dioxus desktop UI (no tray)" }

            div { style: "display: grid; grid-template-columns: 120px 1fr; gap: 10px; align-items: center; margin-bottom: 12px;",
                label { "Target URL" }
                input {
                    value: "{url}",
                    oninput: move |evt| url.set(evt.value()),
                    style: "width: 100%; padding: 8px; border: 1px solid #ccc; border-radius: 8px;",
                }

                label { "Threshold" }
                input {
                    value: "{threshold}",
                    oninput: move |evt| {
                        if let Ok(v) = evt.value().parse::<f64>() {
                            threshold.set(v);
                        }
                    },
                    style: "width: 140px; padding: 8px; border: 1px solid #ccc; border-radius: 8px;",
                }
            }

            div { style: "display: flex; gap: 10px; margin-bottom: 12px;",
                button {
                    onclick: on_start,
                    disabled: running(),
                    style: "padding: 8px 12px; border-radius: 10px; border: 1px solid #1a73e8; background: #1a73e8; color: white;",
                    "Start"
                }
                button {
                    onclick: on_stop,
                    disabled: !running(),
                    style: "padding: 8px 12px; border-radius: 10px; border: 1px solid #aaa; background: #f5f5f5;",
                    "Stop"
                }
                div { style: "margin-left: auto; padding: 8px 10px; border-radius: 10px; background: #f2f7ff; border: 1px solid #d6e6ff;",
                    strong { "Status: " }
                    span { { if running() { "Running" } else { "Stopped" } } }
                }
            }

            div { style: "border: 1px solid #ddd; border-radius: 12px; padding: 10px; height: 420px; overflow: auto; background: #fff;",
                pre { style: "margin: 0; white-space: pre-wrap; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px;",
                    for line in logs().iter() {
                        "{line}\n"
                    }
                }
            }
        }
    }
}

fn main() {
    launch(App);
}
