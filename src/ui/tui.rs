//! The live dashboard shown by `tgfwd start --tui`.
//!
//! It renders the same counters the log output reports, but arranged so the
//! things that matter while watching a live forwarder — what is in flight, what
//! is rate-limited, and what was rescued from a deletion — are visible at once
//! rather than scrolled past.

use std::sync::Arc;
use std::time::Duration;

use color_eyre::eyre::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};
use ratatui::{DefaultTerminal, Frame};
use tokio::sync::watch;

use crate::engine::stats::{Outcome, Snapshot, Stats};

/// How often the dashboard repaints.
const FRAME_INTERVAL: Duration = Duration::from_millis(250);

/// Run the dashboard until the user quits or `shutdown` fires.
pub async fn run(
    stats: Arc<Stats>,
    mut shutdown: watch::Receiver<bool>,
    shutdown_tx: watch::Sender<bool>,
) -> Result<()> {
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &stats, &mut shutdown, &shutdown_tx).await;
    ratatui::restore();
    result
}

async fn event_loop(
    terminal: &mut DefaultTerminal,
    stats: &Arc<Stats>,
    shutdown: &mut watch::Receiver<bool>,
    shutdown_tx: &watch::Sender<bool>,
) -> Result<()> {
    let (key_tx, mut key_rx) = tokio::sync::mpsc::unbounded_channel();

    // Terminal input is blocking, so it is read on its own thread and forwarded
    // as messages. The thread exits when the channel closes.
    std::thread::spawn(move || {
        loop {
            match event::poll(Duration::from_millis(100)) {
                Ok(true) => match event::read() {
                    Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                        if key_tx.send(key).is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => break,
                },
                Ok(false) => {
                    if key_tx.is_closed() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let mut ticker = tokio::time::interval(FRAME_INTERVAL);

    loop {
        let snapshot = stats.snapshot();
        terminal.draw(|frame| draw(frame, &snapshot))?;

        tokio::select! {
            _ = ticker.tick() => {}

            Some(key) = key_rx.recv() => {
                let quit = matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
                    || (key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL));

                if quit {
                    let _ = shutdown_tx.send(true);
                    return Ok(());
                }
            }

            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(());
                }
            }
        }
    }
}

/// Paint one frame.
fn draw(frame: &mut Frame<'_>, snapshot: &Snapshot) {
    let [header, routes, events] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(6),
        Constraint::Percentage(40),
    ])
    .areas(frame.area());

    frame.render_widget(header_widget(snapshot), header);
    frame.render_widget(routes_widget(snapshot), routes);
    frame.render_widget(events_widget(snapshot), events);
}

/// Top bar: uptime and the numbers that describe current pressure.
fn header_widget(snapshot: &Snapshot) -> Paragraph<'static> {
    let totals = snapshot.totals();

    let line = Line::from(vec![
        Span::styled("tgfwd", Style::default().fg(Color::Cyan).bold()),
        Span::raw("  "),
        Span::styled(format_duration(snapshot.uptime), Style::default().dim()),
        Span::raw("   delivered "),
        Span::styled(
            totals.delivered.to_string(),
            Style::default().fg(Color::Green).bold(),
        ),
        Span::raw("   rescued "),
        Span::styled(
            totals.rescued.to_string(),
            Style::default().fg(Color::Magenta).bold(),
        ),
        Span::raw("   failed "),
        Span::styled(
            totals.failed.to_string(),
            if totals.failed > 0 {
                Style::default().fg(Color::Red).bold()
            } else {
                Style::default().dim()
            },
        ),
        Span::raw("   in flight "),
        Span::styled(
            snapshot.in_flight.to_string(),
            Style::default().fg(Color::Yellow),
        ),
        Span::raw("   waiting "),
        Span::styled(
            snapshot.waiting.to_string(),
            if snapshot.waiting > 0 {
                Style::default().fg(Color::Yellow).bold()
            } else {
                Style::default().dim()
            },
        ),
    ]);

    Paragraph::new(line).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" overview  (q to quit) "),
    )
}

/// Per-route table.
fn routes_widget(snapshot: &Snapshot) -> Table<'static> {
    let header = Row::new(vec![
        Cell::from("route"),
        Cell::from("delivered"),
        Cell::from("rescued"),
        Cell::from("failed"),
        Cell::from("filtered"),
        Cell::from("fwd/copy/rehost"),
        Cell::from("latency"),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD | Modifier::DIM));

    let rows: Vec<Row<'static>> = snapshot
        .routes
        .iter()
        .map(|route| {
            let latency = match (route.average_latency, route.worst_latency) {
                (Some(average), Some(worst)) => {
                    format!("{}ms / {}ms", average.as_millis(), worst.as_millis())
                }
                _ => "—".to_owned(),
            };

            Row::new(vec![
                Cell::from(route.route.clone()).style(Style::default().fg(Color::Cyan)),
                Cell::from(route.delivered.to_string()),
                Cell::from(route.rescued.to_string()).style(if route.rescued > 0 {
                    Style::default().fg(Color::Magenta)
                } else {
                    Style::default().dim()
                }),
                Cell::from(route.failed.to_string()).style(if route.failed > 0 {
                    Style::default().fg(Color::Red)
                } else {
                    Style::default().dim()
                }),
                Cell::from(route.filtered.to_string()).style(Style::default().dim()),
                Cell::from(format!(
                    "{}/{}/{}",
                    route.by_forward, route.by_copy, route.by_rehost
                ))
                .style(Style::default().dim()),
                Cell::from(latency).style(Style::default().dim()),
            ])
        })
        .collect();

    Table::new(
        rows,
        [
            Constraint::Min(14),
            Constraint::Length(10),
            Constraint::Length(8),
            Constraint::Length(7),
            Constraint::Length(9),
            Constraint::Length(16),
            Constraint::Min(14),
        ],
    )
    .header(header)
    .block(Block::default().borders(Borders::ALL).title(" routes "))
}

/// Recent activity, newest last so it reads like a log.
fn events_widget(snapshot: &Snapshot) -> Paragraph<'static> {
    let lines: Vec<Line<'static>> = snapshot
        .events
        .iter()
        .rev()
        .take(64)
        .rev()
        .map(|event| {
            let (glyph, color) = match event.outcome {
                Outcome::Delivered => ("✔", Color::Green),
                Outcome::Rescued => ("★", Color::Magenta),
                Outcome::Failed => ("✖", Color::Red),
                Outcome::Filtered => ("·", Color::DarkGray),
            };

            Line::from(vec![
                Span::styled(
                    event.at.format("%H:%M:%S ").to_string(),
                    Style::default().dim(),
                ),
                Span::styled(format!("{glyph} "), Style::default().fg(color)),
                Span::styled(
                    format!("{:<14}", event.route),
                    Style::default().fg(Color::Cyan),
                ),
                Span::raw(event.detail.clone()),
            ])
        })
        .collect();

    Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" recent activity "),
    )
}

/// Render a duration as `1h 02m 03s`, dropping leading zero units.
fn format_duration(duration: Duration) -> String {
    let total = duration.as_secs();
    let (hours, minutes, seconds) = (total / 3600, (total % 3600) / 60, total % 60);

    if hours > 0 {
        format!("{hours}h {minutes:02}m {seconds:02}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_drop_leading_zero_units() {
        assert_eq!(format_duration(Duration::from_secs(9)), "9s");
        assert_eq!(format_duration(Duration::from_secs(65)), "1m 05s");
        assert_eq!(format_duration(Duration::from_secs(3725)), "1h 02m 05s");
    }
}
