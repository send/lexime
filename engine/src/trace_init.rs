#[cfg(feature = "trace")]
use std::path::Path;
#[cfg(feature = "trace")]
use std::sync::Once;

#[cfg(feature = "trace")]
static INIT: Once = Once::new();

#[cfg(feature = "trace")]
pub fn init_tracing(log_dir: &Path) {
    INIT.call_once(|| {
        let file_appender = tracing_appender::rolling::never(log_dir, "lexime-trace.jsonl");
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
        std::mem::forget(guard); // IME is a long-lived process

        tracing_subscriber::fmt()
            .json()
            .with_writer(non_blocking)
            .with_target(true)
            .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("lex_engine=debug")),
            )
            .init();
    });
}

#[cfg(not(feature = "trace"))]
pub fn init_tracing(_log_dir: &std::path::Path) {}
