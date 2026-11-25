//! Logging: `tracing` lines on stdout, where a container runtime or a service manager collects
//! them.
//!
//! Call [`init`] once at the start of `main`, before the first log line. Two environment
//! variables steer it:
//! - `RUST_LOG`: the filter, by default `info` (`RUST_LOG=debug`, `RUST_LOG=my_app=trace,info`);
//! - `LOG_FORMAT`: `json` for one JSON object per line, anything else for readable text.
//!
//! Request spans are an app's own layer (tower-http's `TraceLayer` at level INFO), put around
//! its routes but not around `/healthz`; the README shows it.

use std::io::IsTerminal;

use tracing_subscriber::EnvFilter;

/// Installs the process-wide `tracing` subscriber. Panics when one is already installed.
pub fn init() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    // Colours only on a terminal, not in a container's log or a redirected file.
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(std::io::stdout().is_terminal());
    if wants_json(std::env::var("LOG_FORMAT").ok().as_deref()) {
        builder.json().init();
    } else {
        builder.init();
    }
}

/// Whether `LOG_FORMAT` asks for JSON lines.
fn wants_json(log_format: Option<&str>) -> bool {
    log_format.is_some_and(|format| format.eq_ignore_ascii_case("json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_format_json_is_the_only_switch() {
        assert!(wants_json(Some("json")));
        assert!(wants_json(Some("JSON")));
        for other in [None, Some(""), Some("text"), Some("json "), Some("1")] {
            assert!(!wants_json(other), "{other:?}");
        }
    }

    // `init` itself is tested in tests/telemetry.rs: a process has one subscriber, and the
    // other unit tests should not log through it.
}
