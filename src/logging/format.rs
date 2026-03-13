use tracing_subscriber::fmt;
use tracing_subscriber::EnvFilter;

pub fn init() {
    let _ = tracing_log::LogTracer::init();

    let filter = EnvFilter::try_from_env("ENCLAVE_LOG").unwrap_or_else(|_| EnvFilter::new("info"));

    let subscriber = fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .with_target(true)
        .with_thread_ids(false)
        .with_span_events(fmt::format::FmtSpan::CLOSE)
        .compact()
        .finish();

    let _ = tracing::subscriber::set_global_default(subscriber);
}
