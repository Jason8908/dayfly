use std::io::{self, IsTerminal as _};

use tracing::Subscriber;
use tracing_subscriber::filter::{EnvFilter, FromEnvError, LevelFilter};
use tracing_subscriber::fmt::{self, MakeWriter};
use tracing_subscriber::layer::{Layer, SubscriberExt as _};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::{SubscriberInitExt as _, TryInitError};

use crate::cli::LogFormat;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum InitError {
    #[error("invalid RUST_LOG filter: {0}")]
    Filter(FromEnvError),
    #[error("failed to install the log subscriber")]
    Install(#[source] TryInitError),
}

pub fn init(format: LogFormat) -> Result<(), InitError> {
    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env()
        .map_err(InitError::Filter)?;
    tracing_subscriber::registry()
        .with(filter)
        .with(layer(format, io::stderr, io::stderr().is_terminal()))
        .try_init()
        .map_err(InitError::Install)
}

fn layer<S, W>(
    format: LogFormat,
    writer: W,
    colors_allowed: bool,
) -> Box<dyn Layer<S> + Send + Sync>
where
    S: Subscriber + for<'span> LookupSpan<'span>,
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    let layer = fmt::layer().with_writer(writer);
    // `with_ansi(true)` would override NO_COLOR, which the default already honors.
    let layer = if colors_allowed {
        layer
    } else {
        layer.with_ansi(false)
    };
    match format {
        LogFormat::Pretty => layer.boxed(),
        LogFormat::Json => layer.json().boxed(),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use serde_json::json;

    use super::*;

    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().write(bytes)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn log_inside_pool_and_runner_spans(format: LogFormat) -> String {
        let buffer = Buffer::default();
        let writer = buffer.clone();
        let subscriber =
            tracing_subscriber::registry().with(layer(format, move || writer.clone(), false));

        tracing::subscriber::with_default(subscriber, || {
            let _pool = tracing::info_span!("pool", pool = %"linux").entered();
            let _runner = tracing::info_span!("runner", runner = %"linux-1").entered();
            tracing::info!("runner online");
        });

        String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap()
    }

    #[test]
    fn pretty_lines_carry_pool_and_runner_fields_without_colors() {
        let output = log_inside_pool_and_runner_spans(LogFormat::Pretty);

        assert_eq!(output.lines().count(), 1, "{output}");
        assert!(
            output.contains(" INFO pool{pool=linux}:runner{runner=linux-1}: "),
            "{output}"
        );
        assert!(!output.contains('\x1b'), "{output:?}");
    }

    #[test]
    fn json_lines_carry_pool_and_runner_fields() {
        let output = log_inside_pool_and_runner_spans(LogFormat::Json);

        assert_eq!(output.lines().count(), 1, "{output}");
        let line: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(line["level"], "INFO");
        assert_eq!(line["fields"]["message"], "runner online");
        assert_eq!(
            line["spans"],
            json!([
                {"name": "pool", "pool": "linux"},
                {"name": "runner", "runner": "linux-1"},
            ])
        );
    }
}
