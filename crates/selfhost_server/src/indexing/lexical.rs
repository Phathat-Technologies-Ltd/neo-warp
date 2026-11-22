//! A local BM25 ranker, used when zvec-grep isn't available and for reranking.

use std::collections::HashMap;

const K1: f64 = 1.2;
const B: f64 = 0.75;

/// Lowercased terms of `text`. Identifiers split on punctuation and on case changes, so
/// `parseHttpRequest` and `parse_http_request` share terms; the whole identifier is kept too.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut terms = Vec::new();
    for word in text
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|word| !word.is_empty())
    {
        let parts = split_identifier(word);
        if parts.len() > 1 {
            terms.push(word.to_lowercase());
        }
        terms.extend(parts);
    }
    terms
}

fn split_identifier(word: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut previous_lower = false;
    for c in word.chars() {
        if c == '_' {
            if !current.is_empty() {
                parts.push(std::mem::take(&mut current));
            }
            previous_lower = false;
            continue;
        }
        if c.is_uppercase() && previous_lower && !current.is_empty() {
            parts.push(std::mem::take(&mut current));
        }
        previous_lower = c.is_lowercase() || c.is_ascii_digit();
        current.extend(c.to_lowercase());
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

/// Scores each document against `query` with BM25 and returns document indices, best first.
/// Documents sharing no term with the query are left out.
pub fn rank(query: &str, documents: &[&str]) -> Vec<usize> {
    let query_terms = {
        let mut terms = tokenize(query);
        terms.sort();
        terms.dedup();
        terms
    };
    if query_terms.is_empty() || documents.is_empty() {
        return Vec::new();
    }
    let term_counts: Vec<(HashMap<String, usize>, usize)> = documents
        .iter()
        .map(|document| {
            let terms = tokenize(document);
            let length = terms.len();
            let mut counts = HashMap::new();
            for term in terms {
                *counts.entry(term).or_insert(0) += 1;
            }
            (counts, length)
        })
        .collect();
    let count = documents.len() as f64;
    let average_length =
        (term_counts.iter().map(|(_, length)| *length).sum::<usize>() as f64 / count).max(1.0);
    let idf: Vec<f64> = query_terms
        .iter()
        .map(|term| {
            let containing = term_counts
                .iter()
                .filter(|(counts, _)| counts.contains_key(term))
                .count() as f64;
            ((count - containing + 0.5) / (containing + 0.5) + 1.0).ln()
        })
        .collect();
    let mut scored: Vec<(usize, f64)> = term_counts
        .iter()
        .enumerate()
        .filter_map(|(index, (counts, length))| {
            let score: f64 = query_terms
                .iter()
                .zip(&idf)
                .filter_map(|(term, idf)| {
                    let frequency = *counts.get(term)? as f64;
                    let norm = K1 * (1.0 - B + B * *length as f64 / average_length);
                    Some(idf * frequency * (K1 + 1.0) / (frequency + norm))
                })
                .sum();
            (score > 0.0).then_some((index, score))
        })
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    scored.into_iter().map(|(index, _)| index).collect()
}

#[cfg(test)]
#[path = "lexical_tests.rs"]
mod tests;
