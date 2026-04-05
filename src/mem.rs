/// Read current process RSS (Resident Set Size) from /proc/self/status.
/// Returns the value in KiB, or 0 if unavailable.
pub fn rss_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(0)
}

/// Log current memory usage with a label.
pub fn log_mem(label: &str) {
    let rss = rss_kib();
    tracing::info!("[MEM] {label}: {rss} KiB ({:.1} MiB)", rss as f64 / 1024.0);
}

/// Measure memory delta across a closure. Logs before, after, and delta.
pub fn measure<R>(label: &str, f: impl FnOnce() -> R) -> R {
    let before = rss_kib();
    let result = f();
    let after = rss_kib();
    let delta = after as i64 - before as i64;
    let sign = if delta >= 0 { "+" } else { "" };
    tracing::info!(
        "[MEM] {label}: {before} -> {after} KiB ({sign}{delta} KiB / {sign}{:.1} MiB)",
        delta as f64 / 1024.0
    );
    result
}
