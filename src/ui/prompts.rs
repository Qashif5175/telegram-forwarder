//! Interactive prompts.
//!
//! Two rules shape this module.
//!
//! First, **nobody types a chat ID**. Every chat the user picks comes from their
//! own dialog list, searchable by fuzzy match over title, `@username` and ID.
//!
//! Second, prompts are blocking but the program is async, so each prompt runs on
//! a blocking thread. Reading a line from a terminal must never stall the update
//! stream that is racing a publisher's delete.

use color_eyre::eyre::{Result, bail};
use inquire::ui::{Attributes, Color, RenderConfig, StyleSheet, Styled};
use inquire::{Confirm, InquireError, MultiSelect, Password, Select, Text};
use unicode_width::UnicodeWidthStr;

use crate::telegram::auth::LoginPrompt;
use crate::telegram::dialogs::{ChatKind, DialogEntry};

use super::theme;

/// Shared visual configuration for every prompt.
fn render_config() -> RenderConfig<'static> {
    RenderConfig::default()
        .with_prompt_prefix(Styled::new("◆").with_fg(Color::LightCyan))
        .with_answered_prompt_prefix(Styled::new("◇").with_fg(Color::DarkGrey))
        .with_highlighted_option_prefix(Styled::new("❯").with_fg(Color::LightCyan))
        .with_selected_checkbox(Styled::new("◼").with_fg(Color::LightGreen))
        .with_unselected_checkbox(Styled::new("◻").with_fg(Color::DarkGrey))
        .with_help_message(StyleSheet::new().with_fg(Color::DarkGrey))
        .with_answer(
            StyleSheet::new()
                .with_fg(Color::LightCyan)
                .with_attr(Attributes::BOLD),
        )
}

/// The user backed out of a prompt.
///
/// A distinct type rather than a message, so that adding context to the error on
/// the way up cannot turn a cancellation into a crash report.
#[derive(Debug, thiserror::Error)]
#[error("cancelled")]
pub struct Cancelled;

/// Translate inquire's cancellation into an ordinary error.
///
/// Pressing Esc or Ctrl+C is a normal way to leave a prompt, so it should read
/// as "cancelled", not as a crash.
fn map_error(err: InquireError) -> color_eyre::eyre::Report {
    match err {
        InquireError::OperationCanceled | InquireError::OperationInterrupted => Cancelled.into(),
        other => other.into(),
    }
}

/// Whether an error came from the user cancelling a prompt.
///
/// The whole chain is searched: a cancellation that picked up context on its way
/// out is still a cancellation.
pub fn is_cancellation(err: &color_eyre::eyre::Report) -> bool {
    err.chain().any(<dyn std::error::Error>::is::<Cancelled>)
}

/// Run a blocking prompt without stalling the async runtime.
async fn blocking<T, F>(f: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f).await?
}

/// A chat rendered as one aligned row in a picker.
///
/// The `Display` output doubles as the fuzzy-search haystack, so it deliberately
/// carries the username and ID: typing `tech`, `@twtech` or `1001234` all find
/// the same channel.
#[derive(Debug, Clone)]
pub struct ChatChoice {
    entry: DialogEntry,
    /// Display width to pad the title to, so columns line up.
    title_width: usize,
    /// Display width to pad the username column to.
    username_width: usize,
    /// Whether to warn that this chat cannot be posted into.
    flag_unwritable: bool,
}

impl ChatChoice {
    /// The chat this row refers to.
    pub fn into_entry(self) -> DialogEntry {
        self.entry
    }

    /// An icon hinting at the kind of chat.
    fn icon(&self) -> &'static str {
        if theme::unicode_enabled() {
            match self.entry.kind {
                ChatKind::Channel => "📢",
                ChatKind::Group => "👥",
                ChatKind::Bot => "🤖",
                ChatKind::User => "👤",
                ChatKind::SavedMessages => "📌",
            }
        } else {
            match self.entry.kind {
                ChatKind::Channel => "[C]",
                ChatKind::Group => "[G]",
                ChatKind::Bot => "[B]",
                ChatKind::User => "[U]",
                ChatKind::SavedMessages => "[S]",
            }
        }
    }
}

/// Pad `text` on the right to `width` display columns.
///
/// Uses display width rather than character count, because CJK titles occupy two
/// columns per character and would otherwise wreck the alignment.
fn pad(text: &str, width: usize) -> String {
    let current = UnicodeWidthStr::width(text);
    let mut out = text.to_owned();
    for _ in current..width {
        out.push(' ');
    }
    out
}

/// Truncate `text` to at most `width` display columns, adding an ellipsis.
fn truncate(text: &str, width: usize) -> String {
    if UnicodeWidthStr::width(text) <= width {
        return text.to_owned();
    }

    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = UnicodeWidthStr::width(ch.to_string().as_str());
        if used + w > width.saturating_sub(1) {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

impl std::fmt::Display for ChatChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let title = truncate(&self.entry.title, self.title_width);
        write!(f, "{} {}", self.icon(), pad(&title, self.title_width))?;

        let username = match &self.entry.username {
            Some(name) => format!("@{name}"),
            None => "—".to_owned(),
        };
        write!(f, "  {}", pad(&username, self.username_width))?;

        write!(f, "  {}", self.entry.id)?;

        if self.flag_unwritable {
            write!(f, "  (no post rights)")?;
        }

        Ok(())
    }
}

/// Build aligned choices from raw entries.
///
/// Column widths are computed across the whole set so every row lines up, and
/// are capped so a single absurdly long title cannot push the ID off-screen.
fn build_choices(entries: Vec<DialogEntry>, flag_unwritable: bool) -> Vec<ChatChoice> {
    const MAX_TITLE: usize = 38;
    const MAX_USERNAME: usize = 20;

    let title_width = entries
        .iter()
        .map(|entry| UnicodeWidthStr::width(entry.title.as_str()))
        .max()
        .unwrap_or(0)
        .min(MAX_TITLE);

    let username_width = entries
        .iter()
        .map(|entry| {
            entry
                .username
                .as_ref()
                .map_or(1, |name| UnicodeWidthStr::width(name.as_str()) + 1)
        })
        .max()
        .unwrap_or(1)
        .min(MAX_USERNAME);

    entries
        .into_iter()
        .map(|entry| ChatChoice {
            flag_unwritable: flag_unwritable && !entry.likely_writable,
            entry,
            title_width,
            username_width,
        })
        .collect()
}

/// Let the user pick any number of chats.
///
/// `preselected` marks chats that should start checked, which is what makes
/// *editing* an existing route feel like editing rather than re-entering.
pub async fn pick_chats(
    message: impl Into<String>,
    entries: Vec<DialogEntry>,
    preselected: &[i64],
    warn_unwritable: bool,
) -> Result<Vec<DialogEntry>> {
    if entries.is_empty() {
        bail!("no chats available to choose from");
    }

    let message = message.into();
    let preselected: Vec<i64> = preselected.to_vec();

    blocking(move || {
        let choices = build_choices(entries, warn_unwritable);
        let defaults: Vec<usize> = choices
            .iter()
            .enumerate()
            .filter(|(_, choice)| preselected.contains(&choice.entry.id))
            .map(|(index, _)| index)
            .collect();

        let selected = MultiSelect::new(&message, choices)
            .with_render_config(render_config())
            .with_default(&defaults)
            .with_page_size(12)
            .with_help_message(
                "type to search · ↑↓ move · space select · → all · ← none · enter confirm",
            )
            .prompt()
            .map_err(map_error)?;

        Ok(selected.into_iter().map(ChatChoice::into_entry).collect())
    })
    .await
}

/// Ask a free-text question.
pub async fn text(message: impl Into<String>, help: Option<&str>) -> Result<String> {
    edit_text(message, help, String::new()).await
}

/// Ask a free-text question with `current` already in the buffer.
///
/// Editing anything means starting from what is already configured. Re-typing a
/// value the file already holds is exactly what this tool refuses to ask for
/// elsewhere, and keywords are no easier to recall than chat titles.
pub async fn edit_text(
    message: impl Into<String>,
    help: Option<&str>,
    current: String,
) -> Result<String> {
    let message = message.into();
    let help = help.map(str::to_owned);

    blocking(move || {
        let mut prompt = Text::new(&message).with_render_config(render_config());
        if let Some(help) = &help {
            prompt = prompt.with_help_message(help);
        }
        if !current.is_empty() {
            prompt = prompt.with_initial_value(&current);
        }
        prompt.prompt().map_err(map_error)
    })
    .await
}

/// Ask for a secret, echoing nothing.
pub async fn password(message: impl Into<String>, help: Option<&str>) -> Result<String> {
    let message = message.into();
    let help = help.map(str::to_owned);

    blocking(move || {
        let mut prompt = Password::new(&message)
            .with_render_config(render_config())
            .without_confirmation();
        if let Some(help) = &help {
            prompt = prompt.with_help_message(help);
        }
        prompt.prompt().map_err(map_error)
    })
    .await
}

/// Ask a yes/no question.
pub async fn confirm(message: impl Into<String>, default: bool) -> Result<bool> {
    let message = message.into();

    blocking(move || {
        Confirm::new(&message)
            .with_render_config(render_config())
            .with_default(default)
            .prompt()
            .map_err(map_error)
    })
    .await
}

/// Choose one item from a list of labels.
pub async fn select<T>(message: impl Into<String>, options: Vec<(String, T)>) -> Result<T>
where
    T: Send + 'static,
{
    select_starting_at(message, options, 0).await
}

/// Choose one item, with the cursor already on `start`.
///
/// Used when editing: the current value should be the one under the cursor, so
/// keeping it is a single keystroke.
pub async fn select_starting_at<T>(
    message: impl Into<String>,
    options: Vec<(String, T)>,
    start: usize,
) -> Result<T>
where
    T: Send + 'static,
{
    let message = message.into();

    blocking(move || {
        let labels: Vec<String> = options.iter().map(|(label, _)| label.clone()).collect();
        let chosen = Select::new(&message, labels.clone())
            .with_render_config(render_config())
            .with_starting_cursor(start.min(labels.len().saturating_sub(1)))
            .prompt()
            .map_err(map_error)?;

        let index = labels
            .iter()
            .position(|label| label == &chosen)
            .expect("the chosen label came from this list");

        Ok(options
            .into_iter()
            .nth(index)
            .expect("index came from the same list")
            .1)
    })
    .await
}

/// Choose any number of items from a list of labels.
///
/// `preselected` starts those entries checked, so editing an existing set is a
/// matter of toggling rather than rebuilding it.
pub async fn select_many<T>(
    message: impl Into<String>,
    options: Vec<(String, T)>,
    preselected: &[usize],
) -> Result<Vec<T>>
where
    T: Send + 'static,
{
    let message = message.into();
    let preselected = preselected.to_vec();

    blocking(move || {
        let labels: Vec<String> = options.iter().map(|(label, _)| label.clone()).collect();

        // `raw_prompt` reports the index within the original list, which is what
        // maps a choice back to its value. Matching on the label instead would
        // pick the wrong value the moment two labels ever coincide.
        let chosen = MultiSelect::new(&message, labels)
            .with_render_config(render_config())
            .with_default(&preselected)
            .with_page_size(13)
            .with_help_message("↑↓ move · space select · → all · ← none · enter confirm")
            .raw_prompt()
            .map_err(map_error)?;

        let picked: Vec<usize> = chosen.iter().map(|option| option.index).collect();
        Ok(options
            .into_iter()
            .enumerate()
            .filter(|(index, _)| picked.contains(index))
            .map(|(_, (_, value))| value)
            .collect())
    })
    .await
}

/// Drives the sign-in flow through the terminal.
#[derive(Debug, Default)]
pub struct TerminalLoginPrompt;

impl LoginPrompt for TerminalLoginPrompt {
    async fn phone(&mut self) -> Result<String> {
        let value = text(
            "Phone number",
            Some("international format, e.g. +886912345678"),
        )
        .await?;
        Ok(value.trim().to_owned())
    }

    async fn code(&mut self, retry: bool) -> Result<String> {
        let message = if retry {
            "That code was rejected. Login code"
        } else {
            "Login code"
        };
        let value = text(message, Some("check your other Telegram apps")).await?;
        // People paste codes with spaces or dashes; Telegram wants neither.
        Ok(value
            .chars()
            .filter(char::is_ascii_digit)
            .collect::<String>())
    }

    async fn password(&mut self, hint: Option<&str>, retry: bool) -> Result<String> {
        let message = if retry {
            "That password was rejected. Two-factor password"
        } else {
            "Two-factor password"
        };
        let help = hint.map(|hint| format!("your hint: {hint}"));
        password(message, help.as_deref()).await
    }

    fn notify(&mut self, message: &str) {
        eprintln!(
            "{} {}",
            theme::Level::Info.paint(theme::Level::Info.glyph()),
            message
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(title: &str, username: Option<&str>, id: i64) -> DialogEntry {
        DialogEntry {
            id,
            title: title.to_owned(),
            username: username.map(str::to_owned),
            kind: ChatKind::Channel,
            likely_writable: true,
        }
    }

    #[test]
    fn a_cancellation_survives_being_given_context() {
        use color_eyre::eyre::Context as _;

        let cancelled: color_eyre::eyre::Report = Cancelled.into();
        assert!(is_cancellation(&cancelled));

        // Anything on the way up may add context; that must not turn a plain
        // "cancelled" into a crash report and a non-zero exit code.
        let wrapped = Err::<(), _>(cancelled)
            .wrap_err("while editing the route")
            .unwrap_err();
        assert!(is_cancellation(&wrapped));

        let unrelated = color_eyre::eyre::eyre!("the chat is unknown to this account");
        assert!(!is_cancellation(&unrelated));
    }

    #[test]
    fn padding_uses_display_width_not_char_count() {
        // Four CJK characters occupy eight columns, so padding to ten adds two
        // spaces, not six.
        assert_eq!(pad("台灣科技", 10), "台灣科技  ");
        assert_eq!(UnicodeWidthStr::width(pad("台灣科技", 10).as_str()), 10);
    }

    #[test]
    fn ascii_and_cjk_titles_align_to_the_same_width() {
        let choices = build_choices(
            vec![
                entry("台灣科技新聞", Some("twtech"), -1001),
                entry("Rust Weekly", Some("rustweekly"), -1002),
            ],
            false,
        );

        // Alignment means the ID column starts at the same display column on
        // every row, regardless of how wide the characters before it are.
        let id_columns: Vec<usize> = choices
            .iter()
            .map(|choice| {
                let rendered = choice.to_string();
                let id = choice.entry.id.to_string();
                let byte_offset = rendered.find(&id).expect("the row shows the id");
                UnicodeWidthStr::width(&rendered[..byte_offset])
            })
            .collect();

        assert_eq!(
            id_columns[0], id_columns[1],
            "the id column must start at the same place on every row"
        );
    }

    #[test]
    fn long_titles_are_truncated_with_an_ellipsis() {
        let long = "a".repeat(100);
        let result = truncate(&long, 20);
        assert!(UnicodeWidthStr::width(result.as_str()) <= 20);
        assert!(result.ends_with('…'));
    }

    #[test]
    fn truncation_never_splits_a_wide_character() {
        let result = truncate("台灣科技新聞頻道", 7);
        assert!(UnicodeWidthStr::width(result.as_str()) <= 7);
    }

    #[test]
    fn the_rendered_row_is_searchable_by_id_and_username() {
        let choices = build_choices(
            vec![entry("Tech News", Some("twtech"), -1_001_234_567_890)],
            false,
        );
        let rendered = choices[0].to_string();

        assert!(rendered.contains("Tech News"));
        assert!(rendered.contains("@twtech"));
        assert!(rendered.contains("-1001234567890"));
    }

    #[test]
    fn unwritable_chats_are_flagged_only_when_asked() {
        let mut unwritable = entry("Read Only", None, -1001);
        unwritable.likely_writable = false;

        let flagged = build_choices(vec![unwritable.clone()], true);
        assert!(flagged[0].to_string().contains("no post rights"));

        let unflagged = build_choices(vec![unwritable], false);
        assert!(!unflagged[0].to_string().contains("no post rights"));
    }
}
