use super::*;

const DICT: &str = ";;; comment\nhello HH AH0 L OW1\nhello(2) HH EH0 L OW1\nworld W ER1 L D\nl EH1 L\ns EH1 S\none W AH1 N\ntwo T UW1\npoint P OY1 N T\nfive F AY1 V\n";

fn symbols(tokens: &[G2pToken]) -> Vec<String> {
    tokens
        .iter()
        .map(|token| match token {
            G2pToken::Word(phones) => phones
                .iter()
                .map(|phone| match phone.stress {
                    Some(stress) => format!("{}{stress}", phone.symbol),
                    None => phone.symbol.clone(),
                })
                .collect::<Vec<_>>()
                .join(" "),
            G2pToken::Punct(mark) => mark.to_string(),
        })
        .collect()
}

#[test]
fn looks_up_words_and_keeps_punctuation() {
    let g2p = G2p::from_cmudict(DICT);
    assert_eq!(
        symbols(&g2p.convert("Hello, world.")),
        vec!["HH AH0 L OW1", ",", "W ER1 L D", "."]
    );
}

#[test]
fn uses_the_terminal_lexicon_and_spells_consonant_clusters() {
    let g2p = G2p::from_cmudict(DICT);
    assert_eq!(symbols(&g2p.convert("git")), vec!["G IH1 T"]);
    assert_eq!(symbols(&g2p.convert("ls")), vec!["EH1 L EH1 S"]);
}

#[test]
fn reads_numbers() {
    assert_eq!(number_words("0"), "zero");
    assert_eq!(number_words("42"), "forty two");
    assert_eq!(number_words("100"), "one hundred");
    assert_eq!(number_words("2026"), "two thousand twenty six");
    assert_eq!(number_words("0700"), "zero seven zero zero");
    let g2p = G2p::from_cmudict(DICT);
    assert_eq!(
        symbols(&g2p.convert("1.5")),
        vec!["W AH1 N", "P OY1 N T", "F AY1 V"]
    );
}

#[test]
fn falls_back_to_letter_rules() {
    let phones: Vec<String> = letter_to_sound("zork")
        .into_iter()
        .map(|phone| {
            format!(
                "{}{}",
                phone.symbol,
                phone.stress.map_or(String::new(), |s| s.to_string())
            )
        })
        .collect();
    assert_eq!(phones, vec!["Z", "AO1", "R", "K"]);
}
