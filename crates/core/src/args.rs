//! The extra command-line flags a user gives an app, from the text they typed.

/// The flags open a quote they never close.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnclosedQuote;

/// Splits `text` into arguments: on whitespace, with `"…"` or `'…'` keeping spaces inside one
/// argument (`--name "My Projects"`). A backslash is an ordinary character, so Windows paths
/// (`C:\Users\me`) come through as typed. An unclosed quote is an error, not a guess.
pub fn split(text: &str) -> Result<Vec<String>, UnclosedQuote> {
    Ok(words(text)?.into_iter().map(|w| w.value).collect())
}

/// One argument: as it was typed (quotes and all) and as the app receives it.
struct Word<'a> {
    typed: &'a str,
    value: String,
}

fn words(text: &str) -> Result<Vec<Word<'_>>, UnclosedQuote> {
    let mut words = Vec::new();
    let mut current = String::new();
    // Where the argument being read starts, even an empty one (`""`).
    let mut start: Option<usize> = None;
    let mut quote: Option<char> = None;
    for (at, c) in text.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => current.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                start.get_or_insert(at);
            }
            None if c.is_whitespace() => {
                if let Some(typed) = start.take().and_then(|from| text.get(from..at)) {
                    words.push(Word { typed, value: std::mem::take(&mut current) });
                }
            }
            None => {
                current.push(c);
                start.get_or_insert(at);
            }
        }
    }
    if quote.is_some() {
        return Err(UnclosedQuote);
    }
    if let Some(typed) = start.and_then(|from| text.get(from..)) {
        words.push(Word { typed, value: current });
    }
    Ok(words)
}

/// One place a flag is given: the words it covers and its value.
struct Found {
    at: usize,
    len: usize,
    value: Option<String>,
}

/// Where `flag` is given, up to a `--`: as `--flag`, `--flag=value` or, when it `takes_value`,
/// `--flag value`.
fn find(words: &[Word<'_>], flag: &str, takes_value: bool) -> Vec<Found> {
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(word) = words.get(at) {
        if word.value == "--" {
            break;
        }
        let mut len = 1;
        if word.value == flag {
            let value = words.get(at + 1).filter(|next| takes_value && !next.value.starts_with("--")).map(|next| next.value.clone());
            len += usize::from(value.is_some());
            found.push(Found { at, len, value });
        } else if let Some(value) = word.value.strip_prefix(flag).and_then(|rest| rest.strip_prefix('=')) {
            found.push(Found { at, len, value: Some(value.to_string()) });
        }
        at += len;
    }
    found
}

/// Whether `text` gives `flag`, and its value: `None` when it isn't there, `Some(None)` when it is
/// there bare. The last one counts. Text with an unclosed quote gives nothing.
pub fn flag(text: &str, flag: &str, takes_value: bool) -> Option<Option<String>> {
    find(&words(text).ok()?, flag, takes_value).pop().map(|found| found.value)
}

/// How many times `text` gives `flag`.
pub fn count(text: &str, flag: &str, takes_value: bool) -> usize {
    words(text).map_or(0, |words| find(&words, flag, takes_value).len())
}

/// `value` as one word of launch options.
fn quoted(value: &str) -> String {
    if !value.contains(|c: char| c.is_whitespace() || c == '"' || c == '\'') {
        value.to_string()
    } else if !value.contains('"') {
        format!("\"{value}\"")
    } else {
        // A value with both kinds of quote can't be written: the single ones are dropped.
        format!("'{}'", value.replace('\'', ""))
    }
}

/// `text` with `flag` taken out (`to` is `None`), or given once: bare for `Some(None)` or an empty
/// value. It stays where it was, or goes last (before a `--`). The other arguments are kept as
/// typed.
pub fn set_flag(text: &str, flag: &str, takes_value: bool, to: Option<Option<&str>>) -> Result<String, UnclosedQuote> {
    let words = words(text)?;
    let found = find(&words, flag, takes_value);
    let end = words.iter().position(|w| w.value == "--").unwrap_or(words.len());
    let here = found.first().map_or(end, |f| f.at);
    let mut out: Vec<String> = Vec::new();
    for at in 0..=words.len() {
        if at == here
            && let Some(value) = to
        {
            out.push(flag.to_string());
            out.extend(value.filter(|v| !v.is_empty()).map(quoted));
        }
        if !found.iter().any(|f| (f.at..f.at + f.len).contains(&at)) {
            out.extend(words.get(at).map(|w| w.typed.to_string()));
        }
    }
    Ok(out.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(text: &str) -> Vec<String> {
        split(text).unwrap()
    }

    #[test]
    fn whitespace_separates_arguments() {
        assert_eq!(ok("  --verbose   -x\t1 "), ["--verbose", "-x", "1"]);
        assert!(ok("").is_empty());
        assert!(ok("   ").is_empty());
    }

    #[test]
    fn quotes_keep_spaces_and_join_with_what_touches_them() {
        assert_eq!(ok(r#"--dir "My Projects" 'a b'"#), ["--dir", "My Projects", "a b"]);
        assert_eq!(ok(r#"--name="A B""#), ["--name=A B"]);
        assert_eq!(ok(r#""it's" 'say "hi"'"#), ["it's", r#"say "hi""#]);
        assert_eq!(ok(r#""" x"#), ["", "x"]);
    }

    #[test]
    fn backslashes_are_kept_for_windows_paths() {
        assert_eq!(ok(r"--open C:\Users\me\file.txt"), ["--open", r"C:\Users\me\file.txt"]);
    }

    #[test]
    fn an_unclosed_quote_is_an_error() {
        assert_eq!(split(r#"--dir "My Projects"#), Err(UnclosedQuote));
        assert_eq!(split("'"), Err(UnclosedQuote));
    }

    #[test]
    fn a_flag_is_read_with_its_value_up_to_the_separator() {
        assert_eq!(flag("--control 7878 --safe-gpu", "--control", true), Some(Some("7878".into())));
        assert_eq!(flag("--control=7879", "--control", true), Some(Some("7879".into())));
        assert_eq!(flag("--control --safe-gpu", "--control", true), Some(None));
        assert_eq!(flag("--safe-gpu file", "--safe-gpu", false), Some(None));
        assert_eq!(flag("--control-token x", "--control", true), None);
        assert_eq!(flag("-- --control 1", "--control", true), None);
        assert_eq!(flag("--control '1", "--control", true), None);
    }

    #[test]
    fn setting_a_flag_keeps_the_other_arguments_as_typed() {
        let text = r#"--dir "My Docs" --control 1 'a b'"#;
        assert_eq!(set_flag(text, "--control", true, Some(Some("2"))).unwrap(), r#"--dir "My Docs" --control 2 'a b'"#);
        assert_eq!(set_flag(text, "--control", true, None).unwrap(), r#"--dir "My Docs" 'a b'"#);
        assert_eq!(set_flag("a -- b", "--safe-gpu", false, Some(None)).unwrap(), "a --safe-gpu -- b");
        assert_eq!(set_flag("--control=1 --control 2", "--control", true, Some(Some(""))).unwrap(), "--control");
        assert_eq!(set_flag("--x '", "--control", true, None), Err(UnclosedQuote));
    }

    #[test]
    fn a_value_with_spaces_or_a_quote_is_read_back_as_one() {
        for value in [r"C:\My Files\key", "it's here", r#"say "hi""#] {
            let text = set_flag("", "--control-token-file", true, Some(Some(value))).unwrap();
            assert_eq!(flag(&text, "--control-token-file", true), Some(Some(value.into())), "{text}");
        }
    }
}
