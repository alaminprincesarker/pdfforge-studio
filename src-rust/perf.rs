use serde::Serialize;
use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RustPerfSpan {
    pub op: String,
    pub ms: f64,
    pub detail: String,
}

struct PerfState {
    start: Instant,
    window_visible_ms: Option<f64>,
    ui_ready_ms: Option<f64>,
    spans: VecDeque<RustPerfSpan>,
}

static PERF_STATE: OnceLock<Mutex<PerfState>> = OnceLock::new();

fn state() -> &'static Mutex<PerfState> {
    PERF_STATE.get_or_init(|| {
        Mutex::new(PerfState {
            start: Instant::now(),
            window_visible_ms: None,
            ui_ready_ms: None,
            spans: VecDeque::with_capacity(128),
        })
    })
}

pub fn init_process_start() {
    let _ = state();
}

#[allow(dead_code)]
pub fn elapsed_ms() -> f64 {
    let g = state().lock().unwrap();
    (g.start.elapsed().as_secs_f64() * 1000.0 * 10.0).round() / 10.0
}

pub fn mark_window_visible() -> f64 {
    let mut g = state().lock().unwrap();
    let ms = (g.start.elapsed().as_secs_f64() * 1000.0 * 10.0).round() / 10.0;
    if g.window_visible_ms.is_none() {
        g.window_visible_ms = Some(ms);
    }
    g.window_visible_ms.unwrap()
}

pub fn mark_ui_ready() -> (Option<f64>, f64) {
    let mut g = state().lock().unwrap();
    let ms = (g.start.elapsed().as_secs_f64() * 1000.0 * 10.0).round() / 10.0;
    if g.ui_ready_ms.is_none() {
        g.ui_ready_ms = Some(ms);
    }
    (g.window_visible_ms, g.ui_ready_ms.unwrap())
}

pub fn record_span(op: &str, ms: f64, detail: impl Into<String>) {
    let mut g = state().lock().unwrap();
    if g.spans.len() >= 120 {
        g.spans.pop_front();
    }
    g.spans.push_back(RustPerfSpan {
        op: op.to_string(),
        ms: (ms * 10.0).round() / 10.0,
        detail: detail.into(),
    });
}

#[cfg(target_os = "windows")]
pub fn process_rss_mb() -> f64 {
    #[repr(C)]
    #[allow(non_snake_case)]
    struct ProcessMemoryCounters {
        cb: u32,
        PageFaultCount: u32,
        PeakWorkingSetSize: usize,
        WorkingSetSize: usize,
        QuotaPeakPagedPoolUsage: usize,
        QuotaPagedPoolUsage: usize,
        QuotaPeakNonPagedPoolUsage: usize,
        QuotaNonPagedPoolUsage: usize,
        PagefileUsage: usize,
        PeakPagefileUsage: usize,
    }
    type GetMemFn = unsafe extern "system" fn(isize, *mut ProcessMemoryCounters, u32) -> i32;
    extern "system" {
        fn GetCurrentProcess() -> isize;
    }
    static MEM_FN: OnceLock<Option<(libloading::Library, GetMemFn)>> = OnceLock::new();
    let entry = MEM_FN.get_or_init(|| unsafe {
        if let Ok(lib) = libloading::Library::new("psapi.dll") {
            if let Ok(sym) = lib.get::<GetMemFn>(b"GetProcessMemoryInfo\0") {
                let f: GetMemFn = *sym;
                return Some((lib, f));
            }
        }
        if let Ok(lib) = libloading::Library::new("kernel32.dll") {
            if let Ok(sym) = lib.get::<GetMemFn>(b"K32GetProcessMemoryInfo\0") {
                let f: GetMemFn = *sym;
                return Some((lib, f));
            }
        }
        None
    });
    if let Some((_, f)) = entry {
        unsafe {
            let mut pmc = std::mem::zeroed::<ProcessMemoryCounters>();
            pmc.cb = std::mem::size_of::<ProcessMemoryCounters>() as u32;
            if f(GetCurrentProcess(), &mut pmc, pmc.cb) != 0 {
                let mb = pmc.WorkingSetSize as f64 / (1024.0 * 1024.0);
                return (mb * 10.0).round() / 10.0;
            }
        }
    }
    0.0
}

#[cfg(not(target_os = "windows"))]
pub fn process_rss_mb() -> f64 {
    0.0
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RustPerfMetrics {
    pub uptime_ms: f64,
    pub window_visible_ms: Option<f64>,
    pub ui_ready_ms: Option<f64>,
    pub rust_rss_mb: f64,
    pub spans: Vec<RustPerfSpan>,
}

pub fn snapshot() -> RustPerfMetrics {
    let g = state().lock().unwrap();
    RustPerfMetrics {
        uptime_ms: (g.start.elapsed().as_secs_f64() * 1000.0 * 10.0).round() / 10.0,
        window_visible_ms: g.window_visible_ms,
        ui_ready_ms: g.ui_ready_ms,
        rust_rss_mb: process_rss_mb(),
        spans: g.spans.iter().cloned().collect(),
    }
}

#[derive(Debug, Clone)]
pub struct PerfBenchConfig {
    pub output_json: String,
    pub files: Vec<String>,
    pub scale_factor: Option<f32>,
}

pub fn parse_perf_bench_args() -> Option<PerfBenchConfig> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let pos = args.iter().position(|a| a == "--perf-bench")?;
    let output_json = args.get(pos + 1)?.clone();
    let scale_factor = args.iter().find_map(|a| {
        a.strip_prefix("--bench-scale=").and_then(|v| v.parse::<f32>().ok())
    });
    let files: Vec<String> = args
        .iter()
        .skip(pos + 2)
        .filter(|a| !a.starts_with("--"))
        .cloned()
        .collect();
    Some(PerfBenchConfig {
        output_json,
        files,
        scale_factor,
    })
}
