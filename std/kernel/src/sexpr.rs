// ---------------------------------------------------------------------------
// S-expression reader/printer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum Val {
    Sym(String),
    Str(String),
    List(Vec<Val>),
    Nil,
}

impl core::fmt::Display for Val {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Val::Sym(s) => write!(f, "{s}"),
            Val::Str(s) => write!(f, "\"{s}\""),
            Val::List(items) => {
                write!(f, "(")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{item}")?;
                }
                write!(f, ")")
            }
            Val::Nil => write!(f, "nil"),
        }
    }
}

pub fn read(input: &str) -> Result<Val, String> {
    let tokens = tokenize(input)?;
    if tokens.is_empty() {
        return Err("empty input".into());
    }
    let (val, rest) = parse_tokens(&tokens)?;
    if !rest.is_empty() {
        return Err("unexpected tokens after expression".into());
    }
    Ok(val)
}

pub fn tokenize(input: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            ' ' | '\t' | '\r' | '\n' => {
                chars.next();
            }
            '(' | ')' => {
                tokens.push(c.to_string());
                chars.next();
            }
            '"' => {
                chars.next();
                let mut s = String::new();
                loop {
                    match chars.next() {
                        Some('\\') => match chars.next() {
                            Some(esc) => s.push(esc),
                            None => return Err("unterminated string escape".into()),
                        },
                        Some('"') => break,
                        Some(ch) => s.push(ch),
                        None => return Err("unterminated string".into()),
                    }
                }
                tokens.push(format!("\"{s}\""));
            }
            ';' => {
                // Comment: skip to end of line.
                while chars.peek().is_some_and(|&c| c != '\n') {
                    chars.next();
                }
            }
            _ => {
                let mut atom = String::new();
                while chars
                    .peek()
                    .is_some_and(|&c| !matches!(c, ' ' | '\t' | '\r' | '\n' | '(' | ')' | '"'))
                {
                    atom.push(chars.next().unwrap());
                }
                tokens.push(atom);
            }
        }
    }
    Ok(tokens)
}

pub fn parse_tokens<'a>(tokens: &'a [String]) -> Result<(Val, &'a [String]), String> {
    if tokens.is_empty() {
        return Err("unexpected end of input".into());
    }
    if tokens[0] == "(" {
        let mut items = Vec::new();
        let mut rest = &tokens[1..];
        loop {
            if rest.is_empty() {
                return Err("unclosed parenthesis".into());
            }
            if rest[0] == ")" {
                return Ok((Val::List(items), &rest[1..]));
            }
            let (val, new_rest) = parse_tokens(rest)?;
            items.push(val);
            rest = new_rest;
        }
    } else if tokens[0] == ")" {
        Err("unexpected )".into())
    } else if tokens[0].starts_with('"') {
        let s = &tokens[0][1..tokens[0].len() - 1];
        Ok((Val::Str(s.to_string()), &tokens[1..]))
    } else if &tokens[0] == "nil" {
        Ok((Val::Nil, &tokens[1..]))
    } else {
        Ok((Val::Sym(tokens[0].clone()), &tokens[1..]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- tokenizer ---

    #[test]
    fn tokenize_symbol() {
        let tokens = tokenize("hello").unwrap();
        assert_eq!(tokens, vec!["hello"]);
    }

    #[test]
    fn tokenize_parens() {
        let tokens = tokenize("(foo bar)").unwrap();
        assert_eq!(tokens, vec!["(", "foo", "bar", ")"]);
    }

    #[test]
    fn tokenize_string() {
        let tokens = tokenize("\"hello world\"").unwrap();
        assert_eq!(tokens, vec!["\"hello world\""]);
    }

    #[test]
    fn tokenize_string_with_escape() {
        let tokens = tokenize(r#""hello \"world\"""#).unwrap();
        assert_eq!(tokens, vec!["\"hello \"world\"\""]);
    }

    #[test]
    fn tokenize_unterminated_string() {
        assert!(tokenize("\"hello").is_err());
    }

    #[test]
    fn tokenize_unterminated_escape() {
        assert!(tokenize(r#""hello\"#).is_err());
    }

    #[test]
    fn tokenize_comment() {
        let tokens = tokenize("; this is a comment").unwrap();
        assert!(tokens.is_empty());
    }

    #[test]
    fn tokenize_comment_then_code() {
        let tokens = tokenize("; comment\nhello").unwrap();
        assert_eq!(tokens, vec!["hello"]);
    }

    #[test]
    fn tokenize_nested() {
        let tokens = tokenize("(host (id))").unwrap();
        assert_eq!(tokens, vec!["(", "host", "(", "id", ")", ")"]);
    }

    #[test]
    fn tokenize_whitespace_variants() {
        let tokens = tokenize("  a\tb\r\nc  ").unwrap();
        assert_eq!(tokens, vec!["a", "b", "c"]);
    }

    #[test]
    fn tokenize_empty() {
        let tokens = tokenize("").unwrap();
        assert!(tokens.is_empty());
    }

    #[test]
    fn tokenize_only_whitespace() {
        let tokens = tokenize("   \t\n  ").unwrap();
        assert!(tokens.is_empty());
    }

    // --- parser ---

    #[test]
    fn parse_symbol() {
        match read("hello").unwrap() {
            Val::Sym(s) => assert_eq!(s, "hello"),
            other => panic!("expected Sym, got {other:?}"),
        }
    }

    #[test]
    fn parse_string() {
        match read("\"hello\"").unwrap() {
            Val::Str(s) => assert_eq!(s, "hello"),
            other => panic!("expected Str, got {other:?}"),
        }
    }

    #[test]
    fn parse_nil() {
        assert!(matches!(read("nil").unwrap(), Val::Nil));
    }

    #[test]
    fn parse_list() {
        match read("(a b c)").unwrap() {
            Val::List(items) => {
                assert_eq!(items.len(), 3);
                assert!(matches!(&items[0], Val::Sym(s) if s == "a"));
                assert!(matches!(&items[1], Val::Sym(s) if s == "b"));
                assert!(matches!(&items[2], Val::Sym(s) if s == "c"));
            }
            other => panic!("expected List, got {other:?}"),
        }
    }

    #[test]
    fn parse_empty_list() {
        match read("()").unwrap() {
            Val::List(items) => assert!(items.is_empty()),
            other => panic!("expected empty List, got {other:?}"),
        }
    }

    #[test]
    fn parse_nested_list() {
        match read("(host (id))").unwrap() {
            Val::List(items) => {
                assert_eq!(items.len(), 2);
                assert!(matches!(&items[0], Val::Sym(s) if s == "host"));
                match &items[1] {
                    Val::List(inner) => {
                        assert_eq!(inner.len(), 1);
                        assert!(matches!(&inner[0], Val::Sym(s) if s == "id"));
                    }
                    other => panic!("expected inner List, got {other:?}"),
                }
            }
            other => panic!("expected List, got {other:?}"),
        }
    }

    #[test]
    fn parse_mixed_types() {
        match read("(echo \"hello\" nil)").unwrap() {
            Val::List(items) => {
                assert_eq!(items.len(), 3);
                assert!(matches!(&items[0], Val::Sym(s) if s == "echo"));
                assert!(matches!(&items[1], Val::Str(s) if s == "hello"));
                assert!(matches!(&items[2], Val::Nil));
            }
            other => panic!("expected List, got {other:?}"),
        }
    }

    #[test]
    fn parse_unclosed_paren() {
        assert!(read("(a b").is_err());
    }

    #[test]
    fn parse_unexpected_close_paren() {
        assert!(read(")").is_err());
    }

    #[test]
    fn parse_trailing_tokens() {
        assert!(read("a b").is_err());
    }

    #[test]
    fn parse_empty_input() {
        assert!(read("").is_err());
    }

    // --- Display ---

    #[test]
    fn display_sym() {
        assert_eq!(format!("{}", Val::Sym("foo".into())), "foo");
    }

    #[test]
    fn display_str() {
        assert_eq!(format!("{}", Val::Str("bar".into())), "\"bar\"");
    }

    #[test]
    fn display_nil() {
        assert_eq!(format!("{}", Val::Nil), "nil");
    }

    #[test]
    fn display_list() {
        let v = Val::List(vec![
            Val::Sym("host".into()),
            Val::Str("addr".into()),
        ]);
        assert_eq!(format!("{v}"), "(host \"addr\")");
    }

    #[test]
    fn display_empty_list() {
        assert_eq!(format!("{}", Val::List(vec![])), "()");
    }

    #[test]
    fn display_nested() {
        let v = Val::List(vec![
            Val::Sym("a".into()),
            Val::List(vec![Val::Sym("b".into()), Val::Nil]),
        ]);
        assert_eq!(format!("{v}"), "(a (b nil))");
    }

    // --- round-trip ---

    #[test]
    fn roundtrip_simple() {
        let input = "(executor echo \"hello world\")";
        let val = read(input).unwrap();
        let output = format!("{val}");
        assert_eq!(output, input);
    }

    #[test]
    fn roundtrip_nested() {
        let input = "(session host id)";
        let val = read(input).unwrap();
        let output = format!("{val}");
        assert_eq!(output, input);
    }

    // --- session prefix resolution ---

    #[test]
    fn parse_session_prefixed() {
        match read("(session::host id)").unwrap() {
            Val::List(items) => {
                assert_eq!(items.len(), 2);
                assert!(matches!(&items[0], Val::Sym(s) if s == "session::host"));
                assert!(matches!(&items[1], Val::Sym(s) if s == "id"));
            }
            other => panic!("expected List, got {other:?}"),
        }
    }
}
