//! The extra command-line flags a user gives an app, from the text they typed.

/// The flags open a quote they never close.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnclosedQuote;

/// Splits `text` into arguments: on whitespace, with `"…"` or `'…'` keeping spaces inside one
/// argument (`--name "My Projects"`). A backslash is an ordinary character, so Windows paths
/// (`C:\Users\me`) come through as typed. An unclosed quote is an error, not a guess.
pub fn split(text: &str) -> Result<Vec<String>, UnclosedQuote> {
    let mut args = Vec::new();
    let mut current = String::new();
    // Whether `current` is an argument, even an empty one (`""`).
    let mut started = false;
    let mut quote: Option<char> = None;
    for c in text.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => current.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                started = true;
            }
            None if c.is_whitespace() => {
                if started {
                    args.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            None => {
                current.push(c);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return Err(UnclosedQuote);
    }
    if started {
        args.push(current);
    }
    Ok(args)
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
}
