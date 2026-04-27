//! Live telemetry: poll llama-server, FLM/NPU, system info.
//! All updates are pushed into a shared State via tokio channel.

use serde::Deserialize;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use sysinfo::System;
use tokio::time::interval;

#[derive(Debug, Clone, Default)]
pub struct Telemetry {
    pub llama_alive: bool,
    pub llama_model: String,
    pub llama_n_ctx: u32,
    pub flm_alive: bool,
    pub flm_models: Vec<String>,
    pub mem_used_mb: u64,
    pub mem_total_mb: u64,
    pub swap_used_mb: u64,
    pub swap_total_mb: u64,
    pub cpu_pct: f32,
    pub vram_used_mb: u64,
    pub vram_total_mb: u64,
    pub last_tg_tps: f32, // most recent gen rate (tokens/sec)
    pub last_pp_tps: f32, // most recent prompt-eval rate
    pub last_accept_rate: f32,
}

#[derive(Debug, Deserialize)]
struct ModelsResp {
    data: Vec<ModelEntry>,
}

#[derive(Debug, Deserialize)]
struct ModelEntry {
    id: String,
    #[serde(default)]
    meta: Option<ModelMeta>,
}

#[derive(Debug, Deserialize, Default)]
struct ModelMeta {
    #[serde(default)]
    n_ctx_train: u32,
}

pub fn spawn(state: Arc<Mutex<Telemetry>>, llama_url: String, flm_url: String) {
    tokio::spawn(async move {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        let mut tick = interval(Duration::from_millis(1500));
        let mut sys = System::new_all();
        loop {
            tick.tick().await;
            sys.refresh_memory();
            sys.refresh_cpu_usage();
            let mem_used = sys.used_memory() / 1024 / 1024;
            let mem_total = sys.total_memory() / 1024 / 1024;
            let swap_used = sys.used_swap() / 1024 / 1024;
            let swap_total = sys.total_swap() / 1024 / 1024;
            let cpu = sys.global_cpu_usage();

            let (llama_alive, llama_model, n_ctx) = poll_llama(&client, &llama_url).await;
            let (tg_tps, pp_tps) = if llama_alive { poll_llama_metrics(&client, &llama_url).await } else { (0.0, 0.0) };
            let (flm_alive, flm_models) = poll_flm(&client, &flm_url).await;
            // rocm-smi is synchronous (subprocess) — push to a blocking-thread pool.
            let vram = tokio::task::spawn_blocking(poll_rocm_vram).await.unwrap_or((0, 0));
            let (vram_used, vram_total) = vram;

            if let Ok(mut t) = state.lock() {
                t.llama_alive = llama_alive;
                t.llama_model = llama_model;
                t.llama_n_ctx = n_ctx;
                t.flm_alive = flm_alive;
                t.flm_models = flm_models;
                t.mem_used_mb = mem_used;
                t.mem_total_mb = mem_total;
                t.swap_used_mb = swap_used;
                t.swap_total_mb = swap_total;
                t.cpu_pct = cpu;
                t.vram_used_mb = vram_used;
                t.vram_total_mb = vram_total;
                if tg_tps > 0.0 {
                    t.last_tg_tps = tg_tps;
                }
                if pp_tps > 0.0 {
                    t.last_pp_tps = pp_tps;
                }
            }
        }
    });
}

async fn poll_llama(client: &reqwest::Client, base: &str) -> (bool, String, u32) {
    let url = format!("{}/v1/models", base.trim_end_matches('/'));
    match client.get(&url).send().await {
        Ok(r) if r.status().is_success() => {
            if let Ok(parsed) = r.json::<ModelsResp>().await {
                if let Some(m) = parsed.data.first() {
                    let n_ctx = m.meta.as_ref().map(|x| x.n_ctx_train).unwrap_or(0);
                    return (true, m.id.clone(), n_ctx);
                }
            }
            (true, String::from("(unknown)"), 0)
        }
        _ => (false, String::new(), 0),
    }
}

/// Scrape llama-server's Prometheus /metrics. Returns (tg_tps, pp_tps).
/// Server must be started with --metrics. Both 0 if endpoint missing/501.
async fn poll_llama_metrics(client: &reqwest::Client, base: &str) -> (f32, f32) {
    let url = format!("{}/metrics", base.trim_end_matches('/'));
    match client.get(&url).send().await {
        Ok(r) if r.status().is_success() => {
            let body = r.text().await.unwrap_or_default();
            let mut tg = 0.0f32;
            let mut pp = 0.0f32;
            for line in body.lines() {
                let line = line.trim();
                if line.starts_with('#') || line.is_empty() {
                    continue;
                }
                let val: f32 = line.split_whitespace().last().and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0);
                if line.starts_with("llamacpp:predicted_tokens_seconds") {
                    tg = val;
                } else if line.starts_with("llamacpp:prompt_tokens_seconds") {
                    pp = val;
                }
            }
            (tg, pp)
        }
        _ => (0.0, 0.0),
    }
}

async fn poll_flm(client: &reqwest::Client, base: &str) -> (bool, Vec<String>) {
    let url = format!("{}/v1/models", base.trim_end_matches('/'));
    match client.get(&url).send().await {
        Ok(r) if r.status().is_success() => {
            if let Ok(parsed) = r.json::<ModelsResp>().await {
                let models: Vec<String> = parsed.data.into_iter().map(|m| m.id).collect();
                return (true, models);
            }
            (true, Vec::new())
        }
        _ => (false, Vec::new()),
    }
}

fn poll_rocm_vram() -> (u64, u64) {
    // rocm-smi is the source of truth on this hardware. Cheap shell probe.
    use std::process::Command;
    let out = Command::new("rocm-smi")
        .args(["--showmeminfo", "vram", "--csv"])
        .output();
    let Ok(out) = out else { return (0, 0); };
    let s = String::from_utf8_lossy(&out.stdout);
    // Format: "device,VRAM Total Memory (B),VRAM Total Used Memory (B)\ncard0,17179869184,1234567890"
    let mut total = 0u64;
    let mut used = 0u64;
    for line in s.lines().skip(1) {
        let cols: Vec<&str> = line.split(',').collect();
        if cols.len() >= 3 {
            total = cols[1].trim().parse().unwrap_or(0);
            used = cols[2].trim().parse().unwrap_or(0);
            break;
        }
    }
    (used / 1024 / 1024, total / 1024 / 1024)
}

pub fn record_gen(state: &Arc<Mutex<Telemetry>>, tps: f32, pp: f32, accept: f32) {
    if let Ok(mut t) = state.lock() {
        t.last_tg_tps = tps;
        t.last_pp_tps = pp;
        if accept > 0.0 {
            t.last_accept_rate = accept;
        }
    }
}
