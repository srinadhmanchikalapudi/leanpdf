//! Choosing which pages to send to the model, building the prompt, and reading page
//! citations back out of the answer.

use std::collections::HashMap;

pub const SYSTEM_PROMPT: &str = "You are the assistant inside LeanPDF, a PDF reader. Answer the user's question using the excerpts \
from their PDF(s) that are provided with each question.\n\
Rules:\n\
- Use only the excerpts and the conversation so far. If the answer is not in them, say so plainly and suggest what to look for. \
The excerpts can be a subset of a longer document, so do not claim the document lacks something unless the excerpts show it.\n\
- Cite the page for every claim as [p.N]. When excerpts come from several files, cite as [File name p.N].\n\
- Be concise and well organised: short paragraphs or bullet lists. Quote sparingly and exactly.\n\
- The excerpts are text extracted from a PDF, so tables and code may look rough; interpret them sensibly.";

const MAX_PAGE_CHARS: usize = 6000;

const STOP: &[&str] = &[
    "the", "and", "for", "are", "was", "were", "what", "which", "who", "how", "why", "when", "where", "does", "did", "this",
    "that", "these", "those", "its", "with", "from", "about", "can", "could", "please", "tell", "explain", "summarize",
    "summarise", "summary", "describe", "give", "show", "document", "pdf", "page", "pages", "chapter", "section", "you",
    "your", "have", "has", "had", "not", "but", "any", "all", "there", "their", "them", "then", "than", "into", "out",
];

fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

/// The meaningful words of a question (no filler words), for ranking and for the folder search.
pub fn terms(question: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    words(question)
        .into_iter()
        .filter(|w| (w.chars().count() >= 3 || w.chars().all(|c| c.is_ascii_digit())) && !STOP.contains(&w.as_str()))
        .filter(|w| seen.insert(w.clone()))
        .collect()
}

fn cost(text: &str) -> usize {
    text.len().min(MAX_PAGE_CHARS)
}

/// Picks the pages of the open document to send, within `budget` characters, returned in page order.
/// Small documents are sent whole. Otherwise: the page being viewed, then the best BM25 matches for the
/// question; a question with no matching words (for example "summarize this") gets the opening pages
/// plus an even sample across the document.
pub fn select_pages(pages: &[(u32, String)], question: &str, current: u32, budget: usize) -> Vec<usize> {
    if pages.is_empty() {
        return Vec::new();
    }
    let total: usize = pages.iter().map(|(_, t)| cost(t)).sum();
    if total <= budget {
        return (0..pages.len()).collect();
    }

    let q = terms(question);
    let n = pages.len() as f32;
    let toks: Vec<HashMap<String, u32>> = pages
        .iter()
        .map(|(_, t)| {
            let mut m = HashMap::new();
            for w in words(t) {
                *m.entry(w).or_insert(0u32) += 1;
            }
            m
        })
        .collect();
    let lens: Vec<f32> = toks.iter().map(|m| m.values().sum::<u32>() as f32).collect();
    let avg = (lens.iter().sum::<f32>() / n).max(1.0);

    let mut scores = vec![0f32; pages.len()];
    for term in &q {
        let df = toks.iter().filter(|m| m.contains_key(term)).count() as f32;
        if df == 0.0 {
            continue;
        }
        let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
        for (i, m) in toks.iter().enumerate() {
            if let Some(&tf) = m.get(term) {
                let tf = tf as f32;
                scores[i] += idf * tf * 2.5 / (tf + 1.5 * (0.25 + 0.75 * lens[i] / avg));
            }
        }
    }

    let mut chosen: Vec<usize> = Vec::new();
    let mut used = 0usize;
    let mut take = |i: usize, chosen: &mut Vec<usize>, used: &mut usize| -> bool {
        if chosen.contains(&i) {
            return true;
        }
        let c = cost(&pages[i].1);
        if *used + c > budget && !chosen.is_empty() {
            return false;
        }
        chosen.push(i);
        *used += c;
        true
    };

    if let Some(i) = pages.iter().position(|(p, _)| *p == current) {
        take(i, &mut chosen, &mut used);
    }
    let mut ranked: Vec<usize> = (0..pages.len()).filter(|&i| scores[i] > 0.0).collect();
    ranked.sort_by(|&a, &b| scores[b].partial_cmp(&scores[a]).unwrap_or(std::cmp::Ordering::Equal));
    for i in ranked.iter().copied() {
        if !take(i, &mut chosen, &mut used) {
            break;
        }
    }

    if ranked.len() < 3 {
        // little to go on: the opening pages (title, contents, abstract) and an even sample
        for i in 0..pages.len().min(3) {
            if !take(i, &mut chosen, &mut used) {
                break;
            }
        }
        let want = ((budget.saturating_sub(used)) / MAX_PAGE_CHARS.min(3000)).max(1);
        let step = (pages.len() / want).max(1);
        let mut i = 0;
        while i < pages.len() {
            if !take(i, &mut chosen, &mut used) {
                break;
            }
            i += step;
        }
    }
    chosen.sort_unstable();
    chosen
}

pub struct Excerpt {
    pub file: String,
    pub path: String,
    /// 0-based
    pub page: u32,
    pub text: String,
}

/// The question plus the excerpts, as the user turn that is sent to the model.
pub fn user_prompt(question: &str, excerpts: &[Excerpt]) -> String {
    let mut s = String::from("Excerpts from the user's PDF(s):\n\n");
    for e in excerpts {
        let t: String = e.text.chars().take(MAX_PAGE_CHARS).collect();
        s.push_str(&format!("<excerpt file=\"{}\" page=\"{}\">\n{}\n</excerpt>\n\n", e.file, e.page + 1, t.trim()));
    }
    s.push_str("Question: ");
    s.push_str(question.trim());
    s
}

// ------------------------------------------------------------------ citations

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cite {
    pub file: Option<String>,
    /// 1-based, as written in the answer
    pub page: u32,
}

/// Page references like [p.12], [pp.12-14], [p.3, p.9] and [Name.pdf p.12] found in an answer.
pub fn parse_citations(text: &str) -> Vec<Cite> {
    let mut out: Vec<Cite> = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'[' {
            i += 1;
            continue;
        }
        let Some(rel_end) = text[i + 1..].find(']') else { break };
        let inner = &text[i + 1..i + 1 + rel_end];
        i += rel_end + 2;
        if inner.len() > 160 {
            continue;
        }
        let lower = inner.to_lowercase();
        // first "p." that starts a word
        let mut at = None; // (where the label starts, where its numbers start)
        for (pos, _) in lower.match_indices("p.") {
            let mut before = lower[..pos].chars().rev();
            let first = before.next();
            if first.map_or(true, |c| !c.is_alphanumeric()) {
                at = Some((pos, pos + 2));
                break;
            }
            // "pp." (a range label)
            if first == Some('p') && before.next().map_or(true, |c| !c.is_alphanumeric()) {
                at = Some((pos - 1, pos + 2));
                break;
            }
        }
        let Some((pos, num_start)) = at else { continue };
        let file = inner[..pos].trim().trim_end_matches([',', ';', ':', '·', '—', '-', ' ']).trim().to_string();
        let file = if file.chars().count() >= 2 { Some(file) } else { None };
        let rest = &lower[num_start..];
        // numbers (and ranges) after the first p.
        let mut num = String::new();
        let mut nums: Vec<u32> = Vec::new();
        let mut flush = |num: &mut String, nums: &mut Vec<u32>| {
            if let Ok(n) = num.parse::<u32>() {
                nums.push(n);
            }
            num.clear();
        };
        for c in rest.chars() {
            if c.is_ascii_digit() {
                num.push(c);
            } else if !num.is_empty() {
                flush(&mut num, &mut nums);
            }
        }
        flush(&mut num, &mut nums);
        // a range such as 12-14 contributes its first page
        let range = rest.contains('-') || rest.contains('–');
        let pages: Vec<u32> = if range && nums.len() == 2 { vec![nums[0]] } else { nums };
        for p in pages.into_iter().filter(|p| *p > 0) {
            let c = Cite { file: file.clone(), page: p };
            if !out.contains(&c) {
                out.push(c);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms_drop_filler() {
        assert_eq!(terms("What does the contract say about renewal?"), ["contract", "say", "renewal"]);
    }

    #[test]
    fn small_documents_are_sent_whole() {
        let pages: Vec<(u32, String)> = (0..5).map(|i| (i, format!("page {i} text"))).collect();
        assert_eq!(select_pages(&pages, "anything", 0, 10_000), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn large_documents_get_matching_pages_and_the_current_one() {
        let mut pages: Vec<(u32, String)> = (0..100).map(|i| (i, format!("filler words {} ", i).repeat(300))).collect();
        pages[42].1 = "the indemnification clause limits liability ".repeat(100);
        pages[77].1 = "indemnification indemnification obligations ".repeat(100);
        let picked = select_pages(&pages, "Explain the indemnification clause", 10, 20_000);
        let nums: Vec<u32> = picked.iter().map(|&i| pages[i].0).collect();
        assert!(nums.contains(&42) && nums.contains(&77), "{nums:?}");
        assert!(nums.contains(&10), "current page is included: {nums:?}");
        assert!(picked.windows(2).all(|w| w[0] < w[1]), "page order");
        assert!(picked.len() < 100);
    }

    #[test]
    fn questions_without_matches_get_an_overview() {
        let pages: Vec<(u32, String)> = (0..200).map(|i| (i, "lorem ipsum ".repeat(500))).collect();
        let picked = select_pages(&pages, "summarize this", 150, 30_000);
        assert!(picked.len() > 3 && picked.len() < 200);
        assert!(picked.contains(&0), "opening page");
    }

    #[test]
    fn citations_are_found() {
        let c = parse_citations("See [p.12] and [pp.14-16], also [Big Book.pdf p. 7, p.9] but not [1] or [x].");
        assert_eq!(c[0], Cite { file: None, page: 12 });
        assert_eq!(c[1], Cite { file: None, page: 14 });
        assert!(c.contains(&Cite { file: Some("Big Book.pdf".into()), page: 7 }));
        assert!(c.contains(&Cite { file: Some("Big Book.pdf".into()), page: 9 }));
        assert_eq!(c.len(), 4);
    }
}
