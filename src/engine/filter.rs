//! Classifying messages and deciding whether they should be forwarded.
//!
//! Everything here is a pure function of already-fetched data: filtering must
//! never require a network round trip, because it runs on the hot path while a
//! publisher may be about to delete the message.

use grammers_client::media::Media;

use crate::config::{Filter, MediaKind};

/// Classify a message's payload into a [`MediaKind`].
///
/// Telegram models several distinct user-facing types as the same "document"
/// with different attributes, so voice notes, round videos, GIFs and plain
/// videos all have to be told apart by inspecting those attributes.
pub fn classify(media: Option<&Media>) -> MediaKind {
    use grammers_tl_types::enums::DocumentAttribute as Attr;

    let Some(media) = media else {
        return MediaKind::Text;
    };

    match media {
        Media::Photo(_) => MediaKind::Photo,
        Media::Sticker(_) => MediaKind::Sticker,
        Media::Poll(_) => MediaKind::Poll,
        Media::Contact(_) => MediaKind::Contact,
        Media::Geo(_) | Media::GeoLive(_) | Media::Venue(_) => MediaKind::Geo,
        // A link preview carries no payload of its own; the message is still text.
        Media::WebPage(_) => MediaKind::Text,
        Media::Dice(_) => MediaKind::Other,

        Media::Document(document) => {
            let Some(grammers_tl_types::enums::Document::Document(raw)) =
                document.raw.document.as_ref()
            else {
                return MediaKind::Document;
            };

            // Attribute order is not guaranteed, so scan for the most specific
            // marker rather than trusting the first one seen.
            let mut kind = MediaKind::Document;
            for attribute in &raw.attributes {
                match attribute {
                    Attr::Audio(audio) => {
                        return if audio.voice {
                            MediaKind::Voice
                        } else {
                            MediaKind::Audio
                        };
                    }
                    Attr::Video(video) => {
                        return if video.round_message {
                            MediaKind::VideoNote
                        } else {
                            MediaKind::Video
                        };
                    }
                    Attr::Animated => kind = MediaKind::Animation,
                    Attr::Sticker(_) => return MediaKind::Sticker,
                    _ => {}
                }
            }
            kind
        }

        // `Media` is `non_exhaustive`: Telegram keeps adding payload types, and
        // an unknown one should still be forwardable rather than a build break.
        _ => MediaKind::Other,
    }
}

/// The facts a filter decision is made from.
///
/// Keeping this separate from the Telegram types is what makes the decision
/// logic testable without constructing protocol objects.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// Message text, or the media caption.
    pub text: String,
    /// What the message carries.
    pub kind: MediaKind,
    /// Whether the message is itself a forward from somewhere else.
    pub is_forward: bool,
}

/// Why a message was not forwarded, for logging and statistics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    /// No configured keyword was present.
    MissingKeyword,
    /// A blocked keyword was present.
    BlockedKeyword,
    /// The media kind is not in the allow-list.
    UnwantedKind,
    /// The message has no media but the route requires it.
    NoMedia,
    /// The message is a forward and the route skips those.
    IsForward,
}

impl Rejection {
    /// A short reason suitable for a log line.
    pub fn reason(self) -> &'static str {
        match self {
            Self::MissingKeyword => "no matching keyword",
            Self::BlockedKeyword => "blocked keyword",
            Self::UnwantedKind => "media kind not allowed",
            Self::NoMedia => "no media",
            Self::IsForward => "is a forward",
        }
    }
}

/// Decide whether `candidate` passes `filter`.
///
/// Conditions are `ANDed`. Keyword matching is case-insensitive substring
/// matching, which is what people expect from a "contains" filter and avoids
/// making users learn a regex dialect.
pub fn evaluate(filter: &Filter, candidate: &Candidate) -> Result<(), Rejection> {
    if filter.skip_forwarded && candidate.is_forward {
        return Err(Rejection::IsForward);
    }

    if filter.require_media && candidate.kind == MediaKind::Text {
        return Err(Rejection::NoMedia);
    }

    if !filter.kinds.is_empty() && !filter.kinds.contains(&candidate.kind) {
        return Err(Rejection::UnwantedKind);
    }

    // Lowercase once rather than per keyword.
    let haystack = candidate.text.to_lowercase();

    if filter
        .exclude
        .iter()
        .any(|word| haystack.contains(&word.to_lowercase()))
    {
        return Err(Rejection::BlockedKeyword);
    }

    if !filter.include.is_empty()
        && !filter
            .include
            .iter()
            .any(|word| haystack.contains(&word.to_lowercase()))
    {
        return Err(Rejection::MissingKeyword);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn candidate(text: &str) -> Candidate {
        Candidate {
            text: text.to_owned(),
            kind: MediaKind::Text,
            is_forward: false,
        }
    }

    #[test]
    fn an_empty_filter_passes_everything() {
        let filter = Filter::default();
        assert!(evaluate(&filter, &candidate("anything at all")).is_ok());
    }

    #[test]
    fn include_requires_at_least_one_keyword() {
        let filter = Filter {
            include: vec!["urgent".to_owned(), "breaking".to_owned()],
            ..Filter::default()
        };

        assert!(evaluate(&filter, &candidate("breaking news")).is_ok());
        assert_eq!(
            evaluate(&filter, &candidate("ordinary news")),
            Err(Rejection::MissingKeyword)
        );
    }

    #[test]
    fn exclude_wins_over_include() {
        let filter = Filter {
            include: vec!["news".to_owned()],
            exclude: vec!["sponsored".to_owned()],
            ..Filter::default()
        };

        assert_eq!(
            evaluate(&filter, &candidate("news, sponsored by someone")),
            Err(Rejection::BlockedKeyword)
        );
    }

    #[test]
    fn an_include_filter_drops_media_that_carries_no_caption() {
        // Worth stating outright, because it surprises people: keywords are
        // matched against the text, and a photo posted without a caption has
        // none. An `include` filter therefore keeps *only* captioned posts.
        let filter = Filter {
            include: vec!["urgent".to_owned()],
            ..Filter::default()
        };

        let captionless = Candidate {
            kind: MediaKind::Photo,
            ..candidate("")
        };
        assert_eq!(
            evaluate(&filter, &captionless),
            Err(Rejection::MissingKeyword)
        );
    }

    #[test]
    fn keywords_match_anywhere_including_inside_longer_words() {
        // Substring matching, not word matching. It is what people expect from
        // "contains" and it is fast, but it does mean a short keyword can match
        // a word that merely contains it.
        let filter = Filter {
            include: vec!["urgent".to_owned()],
            ..Filter::default()
        };

        assert!(evaluate(&filter, &candidate("acting urgently")).is_ok());
        assert!(evaluate(&filter, &candidate("the insurgent group")).is_ok());
    }

    #[test]
    fn keyword_matching_ignores_case() {
        let filter = Filter {
            include: vec!["Urgent".to_owned()],
            ..Filter::default()
        };
        assert!(evaluate(&filter, &candidate("URGENT: read this")).is_ok());
    }

    #[test]
    fn keyword_matching_works_on_chinese_text() {
        let filter = Filter {
            include: vec!["快訊".to_owned()],
            ..Filter::default()
        };
        assert!(evaluate(&filter, &candidate("【快訊】今日重點")).is_ok());
        assert!(evaluate(&filter, &candidate("今日重點")).is_err());
    }

    #[test]
    fn require_media_rejects_plain_text() {
        let filter = Filter {
            require_media: true,
            ..Filter::default()
        };

        assert_eq!(
            evaluate(&filter, &candidate("just text")),
            Err(Rejection::NoMedia)
        );

        let with_photo = Candidate {
            kind: MediaKind::Photo,
            ..candidate("caption")
        };
        assert!(evaluate(&filter, &with_photo).is_ok());
    }

    #[test]
    fn kind_allowlist_is_enforced() {
        let filter = Filter {
            kinds: BTreeSet::from([MediaKind::Photo, MediaKind::Video]),
            ..Filter::default()
        };

        let photo = Candidate {
            kind: MediaKind::Photo,
            ..candidate("")
        };
        let document = Candidate {
            kind: MediaKind::Document,
            ..candidate("")
        };

        assert!(evaluate(&filter, &photo).is_ok());
        assert_eq!(evaluate(&filter, &document), Err(Rejection::UnwantedKind));
    }

    #[test]
    fn forwards_can_be_skipped() {
        let filter = Filter {
            skip_forwarded: true,
            ..Filter::default()
        };
        let forwarded = Candidate {
            is_forward: true,
            ..candidate("relayed")
        };

        assert_eq!(evaluate(&filter, &forwarded), Err(Rejection::IsForward));
    }

    #[test]
    fn text_with_no_media_classifies_as_text() {
        assert_eq!(classify(None), MediaKind::Text);
    }
}
