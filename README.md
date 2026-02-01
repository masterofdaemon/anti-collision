# Anti-Collision Bandwidth Saturator (`two-ip-ru`)

This tool is a specialized network utility designed to maintain internet connection stability by proactively managing bandwidth usage.

## 🎯 The Business Logic (Why this exists)

Some internet connections (especially certain ISP configurations, long-range wireless links, or congested shared channels) suffer from "packet collisions" or degradation when the connection is idle or has low traffic. This manifests as packet loss, high latency, or complete drops during light usage.

**The Solution:** "Jerk" the connection awake. By forcing high-throughput traffic, we force the network hardware (modems, ISP switches) to prioritize the active stream, renegotiate channel parameters, and clear out collision states.

### The Automation Cycle

This tool implements an intelligent state machine to fix the connection *only when necessary*, avoiding wasted bandwidth.

1.  **🔍 Monitoring (Check Phase)**
    - Every cycle, the tool performs a **5-second sample download** from a high-speed target (Cloudflare CDN).
    - It measures the real-time throughput.

2.  **🧠 Decision Logic**
    - **Threshold**: `20 Mbps`
    - **Case A (Healthy)**: If speed is **≥ 20 Mbps**, the connection is considered good.
        - Action: **Sleep for 1 hour**. The tool stays quiet.
    - **Case B (Degraded)**: If speed is **< 20 Mbps**, the connection is considered "colliding" or unstable.
        - Action: Enter **Saturation Mode**.

3.  **🌊 Saturation Mode (The Fix)**
    - The tool opens **4 concurrent, high-speed download streams**.
    - This saturates the bandwidth (typically reaching 100-200+ Mbps).
    - **Recovery Condition**: It keeps saturating until the speed stays above **20 Mbps** for at least **15 continuous seconds**.
    - Once recovered, it returns to the **Sleep** cycle.

## 🚀 Usage

### Installation
Ensure you have [Rust](https://rustup.rs/) installed.
```bash
cargo build --release
```

### Running
Run the binary directly:
```bash
./target/release/two-ip-ru
```
Or via Cargo:
```bash
cargo run --release
```

## Windows builds

The easiest way to produce a Windows `.exe` from macOS is to let GitHub Actions build it.
This repo includes a workflow at `.github/workflows/build.yml` that uploads:

- `two-ip-ru.exe`
- `two-ip-ru-gui.exe`

### Customization
By default, it targets Cloudflare's speed test file. You can override this by passing a URL as an argument:
```bash
./target/release/two-ip-ru https://example.com/large-file.bin
```
When using `cargo run`, remember to pass args after `--`:
```bash
cargo run --release -- https://example.com/large-file.bin
```

## 🛠 Configuration
The behavior is controlled by constants in `src/main.rs`:
- `THRESHOLD_MBPS`: Speed required to be considered "Healthy" (Default: `20.0`).
- `SLEEP_DURATION`: How long to rest after a healthy check (Default: `3600s` / 1 hour).
- `CHECK_DURATION`: Duration of the initial speed sample (Default: `5s`).
- `STREAMS`: Number of parallel download connections (Default: `4`).

## 📊 Logs
The tool logs all actions with timestamps for easy debugging:
```text
[2024-01-31 12:00:00] CHECKING: Measuring current speed (5s sample)...
  Current: 45.20 Mbps
[2024-01-31 12:00:05] OK: Speed is 45.20 Mbps. Sleeping for 1 hour...
```
