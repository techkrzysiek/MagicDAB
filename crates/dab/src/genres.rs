//! Names of the TV-Anytime content genres (ContentCS of ETSI TS 102 822-3-1,
//! the 2011 edition, from the XML of the DVB-I tools), the genres of SPI
//! documents (TS 102 818): genres_table.rs, made by build/genres.py.

#[path = "genres_table.rs"]
mod table;

/// The name of a ContentCS code ("3.6.4.14": "Rock").
pub fn content_name(code: &str) -> Option<&'static str> {
    table::CONTENT.binary_search_by(|(c, _)| (*c).cmp(code)).ok().map(|i| table::CONTENT[i].1)
}

/// A genre as shown: a ContentCS URN or code by its name, one not in the
/// table by the nearest level above it that is ("Era 3.6.16.8"); a name
/// (the text of a genre element) and other schemes as they are.
pub fn genre_label(genre: &str) -> String {
    let code = genre.rsplit(':').next().unwrap_or(genre).trim();
    let content = genre.contains("ContentCS") || (!genre.contains(':') && code.starts_with("3."));
    if !content || !code.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return code.to_string();
    }
    if let Some(n) = content_name(code) {
        return n.to_string();
    }
    let mut up = code;
    while let Some((parent, _)) = up.rsplit_once('.') {
        if let Some(n) = content_name(parent) {
            return format!("{n} {code}");
        }
        up = parent;
    }
    code.to_string()
}

/// The levels above it and the genre itself ("Music › Pop-rock › Rock").
pub fn genre_path(code: &str) -> String {
    let mut parts = Vec::new();
    let mut up = code;
    loop {
        if let Some(n) = content_name(up) {
            parts.push(n);
        }
        match up.rsplit_once('.') {
            Some((p, _)) => up = p,
            None => break,
        }
    }
    parts.reverse();
    parts.join(" › ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn genres_by_their_names() {
        assert!(table::CONTENT.windows(2).all(|w| w[0].0 < w[1].0), "sorted for the search");
        assert_eq!(content_name("3.6.4.14"), Some("Rock"));
        assert_eq!(genre_label("urn:tva:metadata:cs:ContentCS:2004:3.1.1.13"), "Weather forecasts");
        assert_eq!(genre_label("urn:tva:metadata:cs:ContentCS:2005:3.2"), "Sports");
        assert_eq!(genre_label("3.6.16.8"), "Era 3.6.16.8");
        // a name, another scheme: as they are
        assert_eq!(genre_label("Pop"), "Pop");
        assert_eq!(genre_label("urn:tva:metadata:cs:FormatCS:2005:1.2"), "1.2");
        assert_eq!(genre_path("3.6.4.14"), "Music › Pop-rock › Rock");
    }
}
