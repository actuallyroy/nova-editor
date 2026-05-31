// Temporary perf instrumentation: appends timing lines to nova-perf.log next to
// the working dir. Windowed subsystem has no console, so stderr is invisible —
// a file lets us inspect timings after interacting. Remove once profiling is done.

use std::io::Write;

pub fn log(msg: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("nova-perf.log")
    {
        let _ = writeln!(f, "{msg}");
    }
}
