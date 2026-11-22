use super::*;

fn redact_chunks(chunks: &[&str]) -> String {
    let mut redactor = Redactor::new([]);
    let mut out: String = chunks.iter().map(|chunk| redactor.push(chunk)).collect();
    out.push_str(&redactor.flush());
    out
}

fn redact(text: &str) -> String {
    redact_chunks(&[text])
}

#[test]
fn redacts_built_in_secret_shapes() {
    let cases = [
        ("key sk-abcdefghijklmnopqrstuv end", "key [REDACTED] end"),
        ("csk-0123456789abcdefghij", "[REDACTED]"),
        ("AKIAABCDEFGHIJKLMNOP", "[REDACTED]"),
        ("ghp_abcdefghijklmnopqrstuvwxyz0123456789", "[REDACTED]"),
        ("github_pat_11ABCDEFG0123456789abcdef", "[REDACTED]"),
        ("xoxb-1234567890-abcdef", "[REDACTED]"),
        (
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N",
            "[REDACTED]",
        ),
        (
            "Authorization: Bearer abcdef1234567890xyz",
            "Authorization: Bearer [REDACTED]",
        ),
        (
            "curl -H 'bearer abcdefghijklmnop1234'",
            "curl -H 'bearer [REDACTED]'",
        ),
        (
            "git clone https://amit:hunter2pass@example.com/repo",
            "git clone https://amit:[REDACTED]@example.com/repo",
        ),
        (
            "export DB_PASSWORD=hunter2",
            "export DB_PASSWORD=[REDACTED]",
        ),
        ("api_key: \"abc123\"", "api_key: [REDACTED]"),
        (
            "aws_secret_access_key = wJalrXUtnFEMI/K7MDENG",
            "aws_secret_access_key = [REDACTED]",
        ),
        (
            "-----BEGIN RSA PRIVATE KEY-----\nMIIEow\nabc\n-----END RSA PRIVATE KEY-----\nok",
            "[REDACTED]\nok",
        ),
    ];
    for (input, expected) in cases {
        assert_eq!(redact(input), expected, "{input}");
    }
}

#[test]
fn leaves_ordinary_text_alone() {
    let text = "ls -la\r\ntotal 8\r\ndrwxr-xr-x  2 amit staff  64 Mar  1 sk-short\r\n$ ";
    assert_eq!(redact(text), text);
    assert_eq!(
        redact("-----BEGIN CERTIFICATE-----\nabc\n"),
        "-----BEGIN CERTIFICATE-----\nabc\n"
    );
}

#[test]
fn catches_secrets_split_across_chunks() {
    assert_eq!(
        redact_chunks(&["echo ghp_abcdefghij", "klmnopqrstuvwxyz0123456789 done"]),
        "echo [REDACTED] done"
    );
    assert_eq!(
        redact_chunks(&["Authorization: Bea", "rer abcdef12", "34567890xyz\r\n"]),
        "Authorization: Bearer [REDACTED]\r\n"
    );
    assert_eq!(
        redact_chunks(&["export TOKEN=", "abc", "def\n"]),
        "export TOKEN=[REDACTED]\n"
    );
    let chars: Vec<String> = "sk-abcdefghijklmnopqrstuv\n"
        .chars()
        .map(String::from)
        .collect();
    let chars: Vec<&str> = chars.iter().map(String::as_str).collect();
    assert_eq!(redact_chunks(&chars), "[REDACTED]\n");
}

#[test]
fn holds_open_private_key_blocks() {
    let mut redactor = Redactor::new([]);
    assert_eq!(redactor.push("key:\n-----BEGIN OPENSSH "), "key:\n");
    assert_eq!(redactor.push("PRIVATE KEY-----\nb3BlbnNzaC1r\n"), "");
    assert_eq!(redactor.push("-----END OPENSSH "), "");
    assert_eq!(redactor.push("PRIVATE KEY-----\n"), "[REDACTED]\n");
}

#[test]
fn writes_only_what_is_safe_and_flushes_the_rest() {
    let mut redactor = Redactor::new([]);
    assert_eq!(redactor.push("$ echo hel"), "$ echo ");
    assert_eq!(redactor.push("lo "), "hello ");
    assert_eq!(redactor.push("wor"), "");
    assert_eq!(redactor.flush(), "wor");
}

#[test]
fn applies_extra_patterns_and_skips_invalid_ones() {
    let mut redactor = Redactor::new(["corp-[0-9]{4}", "(unclosed"]);
    let mut out = redactor.push("id corp-1234 ok ");
    out.push_str(&redactor.flush());
    assert_eq!(out, "id [REDACTED] ok ");
}
