use super::*;

#[test]
fn identifiers_split_on_case_and_underscores() {
    assert_eq!(
        tokenize("parseHttpRequest(raw_body)"),
        [
            "parsehttprequest",
            "parse",
            "http",
            "request",
            "raw_body",
            "raw",
            "body"
        ]
    );
}

#[test]
fn ranks_matching_documents_first_and_drops_non_matches() {
    let documents = [
        "fn render_tab_bar() {}",
        "fn parse_config(path: &Path) -> Config { toml::from_str(path) }",
        "struct Config { verbose: bool }",
    ];
    assert_eq!(rank("parse config", &documents), [1, 2]);
}

#[test]
fn empty_queries_rank_nothing() {
    assert!(rank("  ", &["anything"]).is_empty());
}
