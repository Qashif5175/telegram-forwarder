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

use std::collections::VecDeque;
use std::fmt;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

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
        .event_format(ConsolaFormat)
        .with_env_filter(filter)
        .with_writer(Deferrable)
        .finish();

    tracing::subscriber::set_global_default(subscriber)?;
    Ok(())
}

/// How many deferred lines to keep before dropping the oldest.
///
/// A long run behind the dashboard could otherwise buffer without limit, and the
/// most recent problems are the ones worth reading afterwards.
const DEFERRED_CAPACITY: usize = 500;

/// Log lines written while the terminal belongs to something else.
static DEFERRED: OnceLock<Mutex<VecDeque<Vec<u8>>>> = OnceLock::new();

/// Whether log output must not touch the terminal right now.
static DEFERRING: AtomicBool = AtomicBool::new(false);

fn deferred() -> &'static Mutex<VecDeque<Vec<u8>>> {
    DEFERRED.get_or_init(|| Mutex::new(VecDeque::new()))
}

/// Hold log output back until [`resume`] is called.
///
/// The dashboard runs on an alternate screen buffer on stdout while `tracing`
/// writes to stderr, and both land on the same terminal: a single warning
/// repaints over the frame and leaves the display in tatters. Suppressing the
/// messages instead would hide exactly the flood waits and delivery failures
/// somebody opens the dashboard to watch, so they are kept and replayed.
pub fn defer() {
    DEFERRING.store(true, Ordering::Release);
}

/// How many lines are currently held back.
///
/// Callers announce the replay before it happens, so the lines do not appear
/// from nowhere after the dashboard closes.
pub fn pending() -> usize {
    deferred().lock().map_or(0, |buffered| buffered.len())
}

/// Resume writing to the terminal, replaying anything captured meanwhile.
pub fn resume() {
    DEFERRING.store(false, Ordering::Release);

    let Ok(mut buffered) = deferred().lock() else {
        return;
    };

    let mut stderr = io::stderr().lock();
    for line in buffered.drain(..) {
        let _ = stderr.write_all(&line);
    }
    let _ = stderr.flush();
}

/// A writer that goes to stderr, or to the deferred buffer while it is held.
#[derive(Debug, Clone, Copy)]
struct Deferrable;

impl io::Write for Deferrable {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !DEFERRING.load(Ordering::Acquire) {
            return io::stderr().write(buf);
        }

        if let Ok(mut buffered) = deferred().lock() {
            buffered.push_back(buf.to_vec());
            while buffered.len() > DEFERRED_CAPACITY {
                buffered.pop_front();
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if DEFERRING.load(Ordering::Acquire) {
            return Ok(());
        }
        io::stderr().flush()
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Deferrable {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        *self
    }
}

/// The event formatter described in the module docs.
#[derive(Debug, Default, Clone, Copy)]
pub struct ConsolaFormat;

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

        let now = chrono::Local::now().format("%H:%M:%S");
        write!(writer, "{} ", theme::dim(&now.to_string()))?;
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

    /// `defer` and `resume` drive process-wide state, so the deferral cases live
    /// in one test rather than racing each other across the test threads.
    #[test]
    fn deferred_lines_are_held_bounded_and_replayed() {
        // The dashboard owns the terminal; a warning written straight to stderr
        // repaints over it. Nothing may be written, and nothing may be lost.
        let mut writer = Deferrable;

        defer();
        writer.write_all(b"first\n").unwrap();
        writer.write_all(b"second\n").unwrap();
        assert_eq!(
            deferred().lock().unwrap().len(),
            2,
            "both lines should be held back"
        );
        assert_eq!(pending(), 2, "both lines should be replayed");
        resume();
        assert!(
            deferred().lock().unwrap().is_empty(),
            "replaying should drain the buffer"
        );

        // A dashboard left open for a week must not accumulate without limit.
        defer();
        for index in 0..(DEFERRED_CAPACITY + 25) {
            writer
                .write_all(format!("line {index}\n").as_bytes())
                .unwrap();
        }
        let held = pending();
        resume();
        assert_eq!(held, DEFERRED_CAPACITY, "the buffer should stay bounded");
    }
}
