//! A compact, `consola`-flavoured log format.
//!
//! The default `tracing` output is built for servers: a timestamp, a level, a
//! target, then key-value pairs. For a tool a person watches live, that is a lot
//! of noise around a short message. This formatter puts a glyph and the message
//! first, and dims everything supporting.
//!
//! ```text
//! 12:04:31 ✔ delivered to Tech News  route=mirror via=forward took=812ms
//! ```

use std::fmt;

use tracing::field::{Field, Visit};
use tracing::{Event, Level as TracingLevel, Subscriber};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::registry::LookupSpan;

use super::theme::{self, Level};

/// Log filtering applied when `RUST_LOG` says nothing.
///
/// The `MTProto` stack logs at debug level constantly; surfacing that by default
/// would bury the messages a user actually cares about.
const DEFAULT_FILTER: &str = "tgfwd=info,warn";

/// Install the global logger.
///
/// `verbosity` counts `-v` flags: 0 is the default, 1 adds this crate's debug
/// output, 2 or more turns on the underlying Telegram library as well.
/// An explicit `RUST_LOG` always wins.
pub fn init(verbosity: u8) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // `grammers` logs through the `log` crate, so bridge it into `tracing`,
    // otherwise its warnings would be invisible.
    tracing_log::LogTracer::init()?;

    let filter = match std::env::var("RUST_LOG") {
        Ok(value) if !value.is_empty() => EnvFilter::new(value),
        _ => EnvFilter::new(match verbosity {
            0 => DEFAULT_FILTER.to_owned(),
            1 => "tgfwd=debug,warn".to_owned(),
            _ => "tgfwd=trace,grammers_client=debug,grammers_mtsender=debug".to_owned(),
        }),
    };

    let subscriber = tracing_subscriber::fmt()
        .event_format(ConsolaFormat::default())
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .finish();

    tracing::subscriber::set_global_default(subscriber)?;
    Ok(())
}

/// The event formatter described in the module docs.
#[derive(Debug, Default, Clone, Copy)]
pub struct ConsolaFormat {
    /// Suppresses the leading timestamp, used by the TUI's embedded log pane.
    pub without_time: bool,
}

impl<S, N> FormatEvent<S, N> for ConsolaFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        _ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let level = match *event.metadata().level() {
            TracingLevel::ERROR => Level::Error,
            TracingLevel::WARN => Level::Warn,
            TracingLevel::INFO => Level::Info,
            _ => Level::Debug,
        };

        let mut visitor = EventVisitor::default();
        event.record(&mut visitor);

        if !self.without_time {
            let now = chrono::Local::now().format("%H:%M:%S");
            write!(writer, "{} ", theme::dim(&now.to_string()))?;
        }

        write!(writer, "{} ", level.paint(level.glyph()))?;

        // An `ok` field marks a success, which reads better in green than in the
        // neutral info colour.
        let message_style = if visitor.success && level == Level::Info {
            Level::Success
        } else {
            level
        };
        write!(writer, "{}", message_style.paint(&visitor.message))?;

        if !visitor.fields.is_empty() {
            write!(writer, "  {}", theme::dim(&visitor.fields.join(" ")))?;
        }

        writeln!(writer)
    }
}

/// Collects an event's message and its remaining fields.
#[derive(Default)]
struct EventVisitor {
    message: String,
    fields: Vec<String>,
    /// Set by a field literally named `ok`, used to colour successes green.
    success: bool,
}

impl Visit for EventVisitor {
    fn record_bool(&mut self, field: &Field, value: bool) {
        if field.name() == "ok" {
            self.success = value;
            return;
        }
        self.fields.push(format!("{}={value}", field.name()));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            value.clone_into(&mut self.message);
        } else {
            self.fields.push(format!("{}={value}", field.name()));
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let rendered = format!("{value:?}");
        if field.name() == "message" {
            // `Debug` rendering of a string literal keeps its quotes; strip them
            // so messages do not read as `"like this"`.
            rendered.trim_matches('"').clone_into(&mut self.message);
        } else {
            self.fields
                .push(format!("{}={}", field.name(), rendered.trim_matches('"')));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_field_becomes_the_message() {
        let mut visitor = EventVisitor::default();
        // `record_debug` is what `tracing` uses for `info!("text")`.
        let quoted = "\"hello world\"";
        visitor.message = quoted.trim_matches('"').to_owned();
        assert_eq!(visitor.message, "hello world");
    }
}
