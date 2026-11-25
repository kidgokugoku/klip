use url::Url;

use crate::model::ClipKind;

pub struct ClassifiedText {
    pub kind: ClipKind,
    pub title: String,
    pub preview_text: String,
    pub color_css: Option<String>,
    pub char_count: u64,
    pub word_count: u64,
}

pub fn classify_text(raw: &str) -> ClassifiedText {
    let trimmed = raw.trim();
    let char_count = raw.chars().count() as u64;
    let word_count = raw.split_whitespace().count() as u64;

    let color_css = parse_color_css(trimmed);
    let kind = if color_css.is_some() {
        ClipKind::Color
    } else if is_http_url(trimmed) {
        ClipKind::Link
    } else {
        ClipKind::Text
    };

    let title = compact_title(trimmed, 120);
    let preview_text = compact_preview(raw, 400);

    ClassifiedText {
        kind,
        title,
        preview_text,
        color_css,
        char_count,
        word_count,
    }
}

pub fn compact_title(text: &str, max_chars: usize) -> String {
    let single_line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    truncate_chars(single_line, max_chars)
}

pub fn compact_preview(text: &str, max_chars: usize) -> String {
    let normalized = text.trim();
    truncate_chars(normalized, max_chars)
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (idx, ch) in s.chars().enumerate() {
        if idx >= max_chars {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

fn is_http_url(s: &str) -> bool {
    if s.is_empty() || s.contains(char::is_whitespace) {
        return false;
    }
    match Url::parse(s) {
        Ok(u) => matches!(u.scheme(), "http" | "https") && u.has_host(),
        Err(_) => false,
    }
}

pub fn parse_color_css(s: &str) -> Option<String> {
    if s.is_empty() || s.len() > 64 || s.contains('\n') {
        return None;
    }
    if let Some(hex) = s.strip_prefix('#') {
        if matches!(hex.len(), 3 | 4 | 6 | 8) && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Some(s.to_lowercase());
        }
        return None;
    }
    let lower = s.to_ascii_lowercase();
    for prefix in ["rgb(", "rgba(", "hsl(", "hsla("] {
        if lower.starts_with(prefix) && lower.ends_with(')') {
            let inner = &lower[prefix.len()..lower.len() - 1];
            let valid_chars = inner
                .bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b',' | b'.' | b'%' | b'/' | b' ' | b'-'));
            if valid_chars && inner.bytes().any(|b| b.is_ascii_digit()) {
                return Some(s.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_links_colors_and_text() {
        let link = classify_text("https://developers.cloudflare.com/r2/");
        assert_eq!(link.kind, ClipKind::Link);

        let hex = classify_text("#FF6B35");
        assert_eq!(hex.kind, ClipKind::Color);
        assert_eq!(hex.color_css.as_deref(), Some("#ff6b35"));

        let rgba = classify_text("rgba(255, 107, 53, 0.85)");
        assert_eq!(rgba.kind, ClipKind::Color);

        let text = classify_text("fn main() {\n    println!(\"hello\");\n}");
        assert_eq!(text.kind, ClipKind::Text);
        assert_eq!(text.title, "fn main() {");
    }
}
