//! `telemetry::init` in a process of its own: it installs the process-wide subscriber.

#[test]
fn init_installs_the_subscriber_once() {
    assert!(!tracing::dispatcher::has_been_set());
    aba_kit::telemetry::init();
    assert!(tracing::dispatcher::has_been_set());
    assert!(
        tracing::enabled!(tracing::Level::INFO),
        "info is the default filter"
    );
    tracing::info!(piece = "telemetry", "a line after init");
    // A second call would mean two parts of an app both think they own the logging.
    assert!(std::panic::catch_unwind(aba_kit::telemetry::init).is_err());
}
