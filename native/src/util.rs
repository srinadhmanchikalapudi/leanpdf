use std::cmp::Ordering;

/// Case-insensitive "natural" order: file2 sorts before file10.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut ai, mut bi) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut na = String::new();
                let mut nb = String::new();
                while let Some(&c) = ai.peek() {
                    if c.is_ascii_digit() {
                        na.push(c);
                        ai.next();
                    } else {
                        break;
                    }
                }
                while let Some(&c) = bi.peek() {
                    if c.is_ascii_digit() {
                        nb.push(c);
                        bi.next();
                    } else {
                        break;
                    }
                }
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let o = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if o != Ordering::Equal {
                    return o;
                }
            }
            (Some(x), Some(y)) => {
                let o = x.to_lowercase().cmp(y.to_lowercase());
                if o != Ordering::Equal {
                    return o;
                }
                ai.next();
                bi.next();
            }
        }
    }
}

pub fn base_name(rel: &str) -> &str {
    rel.rsplit(['/', '\\']).next().unwrap_or(rel)
}

pub fn dir_name(rel: &str) -> &str {
    match rel.rfind('/') {
        Some(i) => &rel[..i],
        None => "",
    }
}

/// "D:20260301103000-05'00'" -> "2026-03-01"
pub fn pdf_date(raw: &str) -> String {
    let digits: String = raw.trim_start_matches("D:").chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.len() >= 8 {
        format!("{}-{}-{}", &digits[0..4], &digits[4..6], &digits[6..8])
    } else {
        raw.to_string()
    }
}

/// Words typed in the search box that should be highlighted inside the PDF.
pub fn terms_from_query(q: &str) -> Vec<String> {
    let t = q.trim();
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        return vec![t[1..t.len() - 1].trim().to_string()];
    }
    t.split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() >= 2).map(str::to_string).collect()
}

/// Opens a web address in the default browser (https only).
pub fn open_url(url: &str) {
    if url.starts_with("https://") {
        let _ = std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order_puts_2_before_10() {
        let mut v = vec!["gamma-10.pdf", "gamma-2.pdf", "Gamma-1.pdf"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["Gamma-1.pdf", "gamma-2.pdf", "gamma-10.pdf"]);
    }

    #[test]
    fn names_and_dirs() {
        assert_eq!(base_name("a/b/c.pdf"), "c.pdf");
        assert_eq!(dir_name("a/b/c.pdf"), "a/b");
        assert_eq!(dir_name("c.pdf"), "");
    }

    #[test]
    fn pdf_dates() {
        assert_eq!(pdf_date("D:20260301103000-05'00'"), "2026-03-01");
        assert_eq!(pdf_date("garbage"), "garbage");
    }

    #[test]
    fn search_terms() {
        assert_eq!(terms_from_query("contract renewal"), ["contract", "renewal"]);
        assert_eq!(terms_from_query("\"payment terms\""), ["payment terms"]);
        assert!(terms_from_query("a").is_empty());
    }
}
