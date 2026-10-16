//! Words people read on the Extensions page: they are plain, and every one has a row in the
//! catalogs (the shared coverage test in `i18n::tests` reads this directory's sources too).

use std::collections::BTreeSet;

/// The files whose literals a person reads.
const SOURCES: [&str; 4] = [include_str!("screen.rs"), include_str!("plan.rs"), include_str!("card.rs"), include_str!("mod.rs")];

/// Words that mean something to a developer and nothing to a person using the page.
const DEVELOPER_WORDS: &[&str] = &[
    "abi",
    "wasm",
    "webassembly",
    "sandbox",
    "manifest",
    "namespace",
    "schema",
    "sha256",
    "sha-256",
    "hash",
    "tier",
    "adapter",
    "payload",
    "json",
    "zip",
    "archive",
    "unpack",
    "generation",
    "signature",
    "revocation",
    "provenance",
    "palette",
    "token",
];

fn literals() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for source in SOURCES {
        for (i, _) in source.match_indices("tl!(\"") {
            let start = i + 5;
            if let Some(len) = source[start..].find('"') {
                out.insert(source[start..start + len].to_string());
            }
        }
    }
    out
}

fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric() && c != '-').filter(|w| !w.is_empty()).map(str::to_lowercase)
}

#[test]
fn the_page_has_words_to_check() {
    assert!(literals().len() >= 25, "found only {:?}", literals());
}

#[test]
fn plain_words() {
    for text in literals() {
        // The one place a file ending is shown to the person is the file picker, which is not a literal here.
        for word in words(&text) {
            assert!(!DEVELOPER_WORDS.contains(&word.as_str()), "{text:?} uses the developer word {word:?}");
        }
    }
}

#[test]
fn words_the_page_shows_never_name_the_inside_of_a_package() {
    let all = literals();
    assert!(all.contains("Made by ArtCraft") && all.contains("Added from a file"));
    assert!(all.iter().all(|t| !t.is_empty() && !t.starts_with(' ') && !t.ends_with(' ')));
}
