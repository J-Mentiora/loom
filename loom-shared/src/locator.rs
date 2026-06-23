//! Locator grammar for the interaction verbs' `--selector` string.
//!
//! Shared by the daemon (parse/validate) and the host shim-manager (resolution).
//! Parsed for *resolution only* — the raw selector string is what feeds
//! `args_canonical_bytes`, so a plain CSS selector hashes byte-identically to
//! today (this parser never rewrites the selector before hashing).
//!
//! Grammar: one or more segments separated by a top-level ` >> `. Each segment
//! is `<prefix>=<value>` where prefix ∈ {css, text, role, frame}. A bare value
//! with no recognized prefix is a single `css=` segment (backward compatible).
//!
//!   css=button#submit
//!   text=Continue
//!   role=button[name="Send"]
//!   frame=iframe[src*="widget"] >> text=Send
//!
//! Scope fence (decisions.md D2): a bare `css=`/`text=`/`role=` resolves only in
//! the top frame + same-origin pierced subframes; crossing an origin boundary
//! REQUIRES an explicit `frame=<css>` segment. (Enforced at resolution time.)

/// One resolution step in a parsed locator chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// CSS selector (the default for a bare, prefix-less selector).
    Css(String),
    /// Visible-text match (substring, case-insensitive, normalized whitespace).
    Text(String),
    /// ARIA role, optionally `role=NAME[name="<accessible name>"]`.
    Role(String),
    /// Descend into the iframe matched by this CSS selector; later segments
    /// resolve inside it. Required to cross an origin boundary (D2).
    Frame(String),
}

/// Why a locator string failed to parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocatorParseError {
    /// A segment used a `prefix=` form whose prefix is not one of
    /// css/text/role/frame.
    UnknownPrefix(String),
    /// The locator string (or a segment) was empty.
    Empty,
}

impl std::fmt::Display for LocatorParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LocatorParseError::UnknownPrefix(p) => write!(
                f,
                "unknown locator prefix `{p}=` (expected css=/text=/role=/frame=)"
            ),
            LocatorParseError::Empty => write!(f, "empty locator"),
        }
    }
}

impl std::error::Error for LocatorParseError {}

/// If `seg` begins with a bare-identifier prefix (`ident=`), return
/// `(prefix, rest)`. Returns `None` when the first non-identifier byte is not
/// `=` — so `button#submit` and `input[name="x"]` (where the first `=` is inside
/// `[]`) are NOT mistaken for prefixes, while `text=…` / `xpath=…` are.
fn segment_prefix(seg: &str) -> Option<(&str, &str)> {
    let bytes = seg.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'=' {
            if i == 0 {
                return None;
            }
            return Some((&seg[..i], &seg[i + 1..]));
        }
        if c.is_ascii_alphanumeric() || c == b'_' || c == b'-' {
            i += 1;
        } else {
            // A non-identifier byte before any `=` ⇒ this is a bare selector
            // (e.g. `button#submit`, `input[name="x"]`), not a `prefix=` form.
            return None;
        }
    }
    None
}

/// Parse a `--selector` string into resolution segments.
///
/// Backward compatibility: a bare selector with no recognized prefix and no
/// top-level ` >> ` parses as a single [`Segment::Css`] — identical resolution
/// behavior to the pre-locator CSS-only path. Segments are joined by ` >> `
/// (space-`>>`-space; not the CSS child combinator `>`).
pub fn parse_locator(raw: &str) -> Result<Vec<Segment>, LocatorParseError> {
    if raw.trim().is_empty() {
        return Err(LocatorParseError::Empty);
    }
    let mut segments = Vec::new();
    for part in raw.split(" >> ") {
        let part = part.trim();
        if part.is_empty() {
            return Err(LocatorParseError::Empty);
        }
        match segment_prefix(part) {
            Some((prefix, rest)) => match prefix {
                "css" => segments.push(Segment::Css(rest.to_string())),
                "text" => segments.push(Segment::Text(rest.to_string())),
                "role" => segments.push(Segment::Role(rest.to_string())),
                "frame" => segments.push(Segment::Frame(rest.to_string())),
                other => return Err(LocatorParseError::UnknownPrefix(other.to_string())),
            },
            // No recognized prefix ⇒ a bare CSS selector (back-compat).
            None => segments.push(Segment::Css(part.to_string())),
        }
    }
    Ok(segments)
}

/// True when the locator is a single bare/`css=` segment — i.e. exactly the
/// pre-locator behavior. Lets callers keep the legacy fast path byte-for-byte.
pub fn is_plain_css(segments: &[Segment]) -> bool {
    matches!(segments, [Segment::Css(_)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_selector_is_a_single_css_segment() {
        // Back-compat: plain CSS must parse to exactly one Css segment so its
        // resolution (and args hash) are unchanged.
        assert_eq!(
            parse_locator("button#submit").unwrap(),
            vec![Segment::Css("button#submit".into())]
        );
        assert!(is_plain_css(&parse_locator("button#submit").unwrap()));
    }

    #[test]
    fn attribute_selector_with_equals_is_css_not_prefix() {
        assert_eq!(
            parse_locator(r#"input[name="email"]"#).unwrap(),
            vec![Segment::Css(r#"input[name="email"]"#.into())]
        );
    }

    #[test]
    fn text_prefix_parses_to_text_segment() {
        assert_eq!(
            parse_locator("text=Continue").unwrap(),
            vec![Segment::Text("Continue".into())]
        );
    }

    #[test]
    fn role_prefix_keeps_name_qualifier() {
        assert_eq!(
            parse_locator(r#"role=button[name="Send"]"#).unwrap(),
            vec![Segment::Role(r#"button[name="Send"]"#.into())]
        );
    }

    #[test]
    fn frame_then_text_composes_left_to_right() {
        assert_eq!(
            parse_locator(r#"frame=iframe[src*="widget"] >> text=Send"#).unwrap(),
            vec![
                Segment::Frame(r#"iframe[src*="widget"]"#.into()),
                Segment::Text("Send".into()),
            ]
        );
    }

    #[test]
    fn css_scope_then_text_composes() {
        assert_eq!(
            parse_locator("css=.list >> text=Item").unwrap(),
            vec![Segment::Css(".list".into()), Segment::Text("Item".into())]
        );
    }

    #[test]
    fn unknown_prefix_is_a_typed_error() {
        assert_eq!(
            parse_locator("xpath=//button"),
            Err(LocatorParseError::UnknownPrefix("xpath".into()))
        );
    }

    #[test]
    fn empty_locator_is_a_typed_error() {
        assert_eq!(parse_locator(""), Err(LocatorParseError::Empty));
    }
}
