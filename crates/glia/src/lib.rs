//! Glia — Clojure-inspired language for wetware.
//!
//! Provides a rich EDN-like data literal language used as both the wetware
//! shell language and configuration format (`.glia` files).
//!
//! # Supported types
//!
//! | Syntax | Val variant | Example |
//! |--------|------------|---------|
//! | `nil` | `Nil` | `nil` |
//! | `true` / `false` | `Bool` | `true` |
//! | integers | `Int` | `42`, `-7` |
//! | floats | `Float` | `3.14`, `1e10` |
//! | `"strings"` | `Str` | `"hello"` |
//! | bare words | `Sym` | `foo`, `bar/baz` |
//! | `:keywords` | `Keyword` | `:port` |
//! | `(lists)` | `List` | `(a b c)` |
//! | `[vectors]` | `Vector` | `[1 2 3]` |
//! | `{maps}` | `Map` | `{:a 1 :b 2}` |
//! | `#{sets}` | `Set` | `#{:a :b}` |
//!
//! Commas are whitespace. Line comments start with `;`.

// ---------------------------------------------------------------------------
// Value type
// ---------------------------------------------------------------------------

/// A Clojure-like value.
#[derive(Debug, Clone)]
pub enum Val {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Sym(String),
    Keyword(String),
    List(Vec<Val>),
    Vector(Vec<Val>),
    Map(Vec<(Val, Val)>),
    Set(Vec<Val>),
}

impl PartialEq for Val {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Val::Nil, Val::Nil) => true,
            (Val::Bool(a), Val::Bool(b)) => a == b,
            (Val::Int(a), Val::Int(b)) => a == b,
            (Val::Float(a), Val::Float(b)) => a.to_bits() == b.to_bits(),
            (Val::Str(a), Val::Str(b)) => a == b,
            (Val::Sym(a), Val::Sym(b)) => a == b,
            (Val::Keyword(a), Val::Keyword(b)) => a == b,
            (Val::List(a), Val::List(b)) => a == b,
            (Val::Vector(a), Val::Vector(b)) => a == b,
            (Val::Map(a), Val::Map(b)) => a == b,
            (Val::Set(a), Val::Set(b)) => a == b,
            _ => false,
        }
    }
}

impl core::fmt::Display for Val {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Val::Nil => write!(f, "nil"),
            Val::Bool(b) => write!(f, "{b}"),
            Val::Int(n) => write!(f, "{n}"),
            Val::Float(n) => {
                // Ensure floats always have a decimal point
                if n.fract() == 0.0 && n.is_finite() {
                    write!(f, "{n:.1}")
                } else {
                    write!(f, "{n}")
                }
            }
            Val::Str(s) => write!(f, "\"{s}\""),
            Val::Sym(s) => write!(f, "{s}"),
            Val::Keyword(s) => write!(f, ":{s}"),
            Val::List(items) => fmt_seq(f, "(", ")", items),
            Val::Vector(items) => fmt_seq(f, "[", "]", items),
            Val::Map(pairs) => {
                write!(f, "{{")?;
                for (i, (k, v)) in pairs.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{k} {v}")?;
                }
                write!(f, "}}")
            }
            Val::Set(items) => fmt_seq(f, "#{", "}", items),
        }
    }
}

fn fmt_seq(
    f: &mut core::fmt::Formatter<'_>,
    open: &str,
    close: &str,
    items: &[Val],
) -> core::fmt::Result {
    write!(f, "{open}")?;
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            write!(f, " ")?;
        }
        write!(f, "{item}")?;
    }
    write!(f, "{close}")
}

// ---------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------

/// Read a single form from `input`.
///
/// Returns an error if the input is empty, malformed, or contains trailing
/// tokens after the first complete form.
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

/// Read all top-level forms from `input`.
///
/// Useful for config files that contain a single data literal or multiple
/// sequential forms.
pub fn read_many(input: &str) -> Result<Vec<Val>, String> {
    let tokens = tokenize(input)?;
    let mut results = Vec::new();
    let mut rest = tokens.as_slice();
    while !rest.is_empty() {
        let (val, remaining) = parse_tokens(rest)?;
        results.push(val);
        rest = remaining;
    }
    Ok(results)
}

// ---------------------------------------------------------------------------
// Tokenizer
// ---------------------------------------------------------------------------

/// Token types produced by the tokenizer.
#[derive(Debug, Clone, PartialEq)]
enum Token {
    Open,     // (
    Close,    // )
    VecOpen,  // [
    VecClose, // ]
    MapOpen,  // {
    MapClose, // }
    SetOpen,  // #{
    Atom(String),
}

fn tokenize(input: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();

    while let Some(&c) = chars.peek() {
        match c {
            // Whitespace (commas are whitespace in Clojure)
            ' ' | '\t' | '\r' | '\n' | ',' => {
                chars.next();
            }
            '(' => {
                tokens.push(Token::Open);
                chars.next();
            }
            ')' => {
                tokens.push(Token::Close);
                chars.next();
            }
            '[' => {
                tokens.push(Token::VecOpen);
                chars.next();
            }
            ']' => {
                tokens.push(Token::VecClose);
                chars.next();
            }
            '{' => {
                tokens.push(Token::MapOpen);
                chars.next();
            }
            '}' => {
                tokens.push(Token::MapClose);
                chars.next();
            }
            '#' => {
                chars.next();
                match chars.peek() {
                    Some('{') => {
                        chars.next();
                        tokens.push(Token::SetOpen);
                    }
                    _ => return Err("unexpected character after #".into()),
                }
            }
            '"' => {
                chars.next();
                let mut s = String::new();
                loop {
                    match chars.next() {
                        Some('\\') => match chars.next() {
                            Some('n') => s.push('\n'),
                            Some('t') => s.push('\t'),
                            Some('\\') => s.push('\\'),
                            Some('"') => s.push('"'),
                            Some(esc) => {
                                s.push('\\');
                                s.push(esc);
                            }
                            None => return Err("unterminated string escape".into()),
                        },
                        Some('"') => break,
                        Some(ch) => s.push(ch),
                        None => return Err("unterminated string".into()),
                    }
                }
                tokens.push(Token::Atom(format!("\"{s}\"")));
            }
            ';' => {
                // Line comment — skip to end of line
                while chars.peek().is_some_and(|&c| c != '\n') {
                    chars.next();
                }
            }
            _ => {
                let mut atom = String::new();
                while chars.peek().is_some_and(|&c| {
                    !matches!(
                        c,
                        ' ' | '\t'
                            | '\r'
                            | '\n'
                            | ','
                            | '('
                            | ')'
                            | '['
                            | ']'
                            | '{'
                            | '}'
                            | '"'
                            | ';'
                    )
                }) {
                    atom.push(chars.next().unwrap());
                }
                tokens.push(Token::Atom(atom));
            }
        }
    }
    Ok(tokens)
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

fn parse_tokens(tokens: &[Token]) -> Result<(Val, &[Token]), String> {
    if tokens.is_empty() {
        return Err("unexpected end of input".into());
    }
    match &tokens[0] {
        Token::Open => parse_seq(&tokens[1..], Token::Close, Val::List),
        Token::VecOpen => parse_seq(&tokens[1..], Token::VecClose, Val::Vector),
        Token::MapOpen => parse_map(&tokens[1..]),
        Token::SetOpen => parse_set(&tokens[1..]),
        Token::Close => Err("unexpected )".into()),
        Token::VecClose => Err("unexpected ]".into()),
        Token::MapClose => Err("unexpected }".into()),
        Token::Atom(a) => Ok((parse_atom(a), &tokens[1..])),
    }
}

fn parse_seq<F>(tokens: &[Token], close: Token, wrap: F) -> Result<(Val, &[Token]), String>
where
    F: FnOnce(Vec<Val>) -> Val,
{
    let mut items = Vec::new();
    let mut rest = tokens;
    loop {
        if rest.is_empty() {
            return Err(format!("unclosed {}", close_name(&close)));
        }
        if rest[0] == close {
            return Ok((wrap(items), &rest[1..]));
        }
        let (val, new_rest) = parse_tokens(rest)?;
        items.push(val);
        rest = new_rest;
    }
}

fn parse_map(tokens: &[Token]) -> Result<(Val, &[Token]), String> {
    let mut pairs = Vec::new();
    let mut rest = tokens;
    loop {
        if rest.is_empty() {
            return Err("unclosed map".into());
        }
        if rest[0] == Token::MapClose {
            return Ok((Val::Map(pairs), &rest[1..]));
        }
        let (key, after_key) = parse_tokens(rest)?;
        if after_key.is_empty() || after_key[0] == Token::MapClose {
            return Err("map must have an even number of elements".into());
        }
        let (val, after_val) = parse_tokens(after_key)?;
        pairs.push((key, val));
        rest = after_val;
    }
}

fn parse_set(tokens: &[Token]) -> Result<(Val, &[Token]), String> {
    let mut items: Vec<Val> = Vec::new();
    let mut rest = tokens;
    loop {
        if rest.is_empty() {
            return Err("unclosed set".into());
        }
        if rest[0] == Token::MapClose {
            return Ok((Val::Set(items), &rest[1..]));
        }
        let (val, new_rest) = parse_tokens(rest)?;
        // Check for duplicates (linear scan — fine for config-sized data)
        if items.iter().any(|existing| existing == &val) {
            return Err(format!("duplicate set element: {val}"));
        }
        items.push(val);
        rest = new_rest;
    }
}

fn close_name(token: &Token) -> &'static str {
    match token {
        Token::Close => "list",
        Token::VecClose => "vector",
        Token::MapClose => "map/set",
        _ => "collection",
    }
}

fn parse_atom(s: &str) -> Val {
    // String literal
    if s.starts_with('"') {
        let inner = &s[1..s.len() - 1];
        return Val::Str(inner.to_string());
    }

    // Keyword
    if let Some(kw) = s.strip_prefix(':') {
        return Val::Keyword(kw.to_string());
    }

    // Reserved words
    match s {
        "nil" => return Val::Nil,
        "true" => return Val::Bool(true),
        "false" => return Val::Bool(false),
        _ => {}
    }

    // Integer
    if let Ok(n) = s.parse::<i64>() {
        return Val::Int(n);
    }

    // Float
    if let Ok(n) = s.parse::<f64>() {
        // Only parse as float if the token looks numeric (not something like "Infinity")
        if s.starts_with(|c: char| c.is_ascii_digit() || c == '-' || c == '+' || c == '.') {
            return Val::Float(n);
        }
    }

    // Symbol (fallback)
    Val::Sym(s.to_string())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- tokenizer ---

    #[test]
    fn tokenize_symbol() {
        let tokens = tokenize("hello").unwrap();
        assert_eq!(tokens, vec![Token::Atom("hello".into())]);
    }

    #[test]
    fn tokenize_parens() {
        let tokens = tokenize("(foo bar)").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Open,
                Token::Atom("foo".into()),
                Token::Atom("bar".into()),
                Token::Close
            ]
        );
    }

    #[test]
    fn tokenize_string() {
        let tokens = tokenize("\"hello world\"").unwrap();
        assert_eq!(tokens, vec![Token::Atom("\"hello world\"".into())]);
    }

    #[test]
    fn tokenize_string_with_escape() {
        let tokens = tokenize(r#""hello \"world\"""#).unwrap();
        assert_eq!(tokens, vec![Token::Atom("\"hello \"world\"\"".into())]);
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
        assert_eq!(tokens, vec![Token::Atom("hello".into())]);
    }

    #[test]
    fn tokenize_nested() {
        let tokens = tokenize("(host (id))").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Open,
                Token::Atom("host".into()),
                Token::Open,
                Token::Atom("id".into()),
                Token::Close,
                Token::Close
            ]
        );
    }

    #[test]
    fn tokenize_whitespace_variants() {
        let tokens = tokenize("  a\tb\r\nc  ").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Atom("a".into()),
                Token::Atom("b".into()),
                Token::Atom("c".into())
            ]
        );
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

    #[test]
    fn tokenize_commas_as_whitespace() {
        let tokens = tokenize("[1, 2, 3]").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::VecOpen,
                Token::Atom("1".into()),
                Token::Atom("2".into()),
                Token::Atom("3".into()),
                Token::VecClose
            ]
        );
    }

    #[test]
    fn tokenize_brackets() {
        let tokens = tokenize("[a b]").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::VecOpen,
                Token::Atom("a".into()),
                Token::Atom("b".into()),
                Token::VecClose
            ]
        );
    }

    #[test]
    fn tokenize_braces() {
        let tokens = tokenize("{:a 1}").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::MapOpen,
                Token::Atom(":a".into()),
                Token::Atom("1".into()),
                Token::MapClose
            ]
        );
    }

    #[test]
    fn tokenize_set() {
        let tokens = tokenize("#{:a :b}").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::SetOpen,
                Token::Atom(":a".into()),
                Token::Atom(":b".into()),
                Token::MapClose
            ]
        );
    }

    #[test]
    fn tokenize_hash_error() {
        assert!(tokenize("#x").is_err());
    }

    // --- parser: atoms ---

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
    fn parse_true() {
        assert!(matches!(read("true").unwrap(), Val::Bool(true)));
    }

    #[test]
    fn parse_false() {
        assert!(matches!(read("false").unwrap(), Val::Bool(false)));
    }

    #[test]
    fn parse_keyword() {
        match read(":port").unwrap() {
            Val::Keyword(k) => assert_eq!(k, "port"),
            other => panic!("expected Keyword, got {other:?}"),
        }
    }

    #[test]
    fn parse_keyword_with_hyphen() {
        match read(":key-file").unwrap() {
            Val::Keyword(k) => assert_eq!(k, "key-file"),
            other => panic!("expected Keyword, got {other:?}"),
        }
    }

    #[test]
    fn parse_integer() {
        assert_eq!(read("42").unwrap(), Val::Int(42));
    }

    #[test]
    fn parse_negative_integer() {
        assert_eq!(read("-7").unwrap(), Val::Int(-7));
    }

    #[test]
    fn parse_zero() {
        assert_eq!(read("0").unwrap(), Val::Int(0));
    }

    #[test]
    fn parse_float() {
        assert_eq!(read("3.14").unwrap(), Val::Float(3.14));
    }

    #[test]
    fn parse_negative_float() {
        assert_eq!(read("-0.5").unwrap(), Val::Float(-0.5));
    }

    #[test]
    fn parse_scientific_notation() {
        assert_eq!(read("1e10").unwrap(), Val::Float(1e10));
    }

    #[test]
    fn parse_scientific_negative_exp() {
        assert_eq!(read("1.5e-3").unwrap(), Val::Float(1.5e-3));
    }

    // --- parser: collections ---

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
    fn parse_vector() {
        match read("[1 2 3]").unwrap() {
            Val::Vector(items) => {
                assert_eq!(items.len(), 3);
                assert_eq!(items[0], Val::Int(1));
                assert_eq!(items[1], Val::Int(2));
                assert_eq!(items[2], Val::Int(3));
            }
            other => panic!("expected Vector, got {other:?}"),
        }
    }

    #[test]
    fn parse_empty_vector() {
        match read("[]").unwrap() {
            Val::Vector(items) => assert!(items.is_empty()),
            other => panic!("expected empty Vector, got {other:?}"),
        }
    }

    #[test]
    fn parse_vector_commas() {
        // Commas are whitespace
        match read("[1, 2, 3]").unwrap() {
            Val::Vector(items) => {
                assert_eq!(items.len(), 3);
                assert_eq!(items[0], Val::Int(1));
            }
            other => panic!("expected Vector, got {other:?}"),
        }
    }

    #[test]
    fn parse_map() {
        match read("{:a 1 :b 2}").unwrap() {
            Val::Map(pairs) => {
                assert_eq!(pairs.len(), 2);
                assert_eq!(pairs[0].0, Val::Keyword("a".into()));
                assert_eq!(pairs[0].1, Val::Int(1));
                assert_eq!(pairs[1].0, Val::Keyword("b".into()));
                assert_eq!(pairs[1].1, Val::Int(2));
            }
            other => panic!("expected Map, got {other:?}"),
        }
    }

    #[test]
    fn parse_empty_map() {
        match read("{}").unwrap() {
            Val::Map(pairs) => assert!(pairs.is_empty()),
            other => panic!("expected empty Map, got {other:?}"),
        }
    }

    #[test]
    fn parse_map_odd_elements() {
        assert!(read("{:a 1 :b}").is_err());
    }

    #[test]
    fn parse_set() {
        match read("#{:a :b :c}").unwrap() {
            Val::Set(items) => {
                assert_eq!(items.len(), 3);
                assert_eq!(items[0], Val::Keyword("a".into()));
                assert_eq!(items[1], Val::Keyword("b".into()));
                assert_eq!(items[2], Val::Keyword("c".into()));
            }
            other => panic!("expected Set, got {other:?}"),
        }
    }

    #[test]
    fn parse_empty_set() {
        match read("#{}").unwrap() {
            Val::Set(items) => assert!(items.is_empty()),
            other => panic!("expected empty Set, got {other:?}"),
        }
    }

    #[test]
    fn parse_set_duplicates() {
        assert!(read("#{:a :b :a}").is_err());
    }

    // --- parser: mixed/nested ---

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
    fn parse_nested_config() {
        let input = r#"{:images ["a" "b"] :flags #{:verbose}}"#;
        match read(input).unwrap() {
            Val::Map(pairs) => {
                assert_eq!(pairs.len(), 2);
                assert_eq!(pairs[0].0, Val::Keyword("images".into()));
                match &pairs[0].1 {
                    Val::Vector(v) => {
                        assert_eq!(v.len(), 2);
                        assert_eq!(v[0], Val::Str("a".into()));
                        assert_eq!(v[1], Val::Str("b".into()));
                    }
                    other => panic!("expected Vector, got {other:?}"),
                }
                assert_eq!(pairs[1].0, Val::Keyword("flags".into()));
                match &pairs[1].1 {
                    Val::Set(s) => {
                        assert_eq!(s.len(), 1);
                        assert_eq!(s[0], Val::Keyword("verbose".into()));
                    }
                    other => panic!("expected Set, got {other:?}"),
                }
            }
            other => panic!("expected Map, got {other:?}"),
        }
    }

    #[test]
    fn parse_config_example() {
        let input = r#"
;; wetware node configuration
{:port     2025
 :key-file "~/.ww/key"
 :images   ["images/my-app" "images/shell"]}
"#;
        match read(input).unwrap() {
            Val::Map(pairs) => {
                assert_eq!(pairs.len(), 3);
                assert_eq!(pairs[0].0, Val::Keyword("port".into()));
                assert_eq!(pairs[0].1, Val::Int(2025));
                assert_eq!(pairs[1].0, Val::Keyword("key-file".into()));
                assert_eq!(pairs[1].1, Val::Str("~/.ww/key".into()));
                assert_eq!(pairs[2].0, Val::Keyword("images".into()));
                match &pairs[2].1 {
                    Val::Vector(v) => assert_eq!(v.len(), 2),
                    other => panic!("expected Vector, got {other:?}"),
                }
            }
            other => panic!("expected Map, got {other:?}"),
        }
    }

    // --- parser: errors ---

    #[test]
    fn parse_unclosed_paren() {
        assert!(read("(a b").is_err());
    }

    #[test]
    fn parse_unexpected_close_paren() {
        assert!(read(")").is_err());
    }

    #[test]
    fn parse_unexpected_close_bracket() {
        assert!(read("]").is_err());
    }

    #[test]
    fn parse_unexpected_close_brace() {
        assert!(read("}").is_err());
    }

    #[test]
    fn parse_trailing_tokens() {
        assert!(read("a b").is_err());
    }

    #[test]
    fn parse_empty_input() {
        assert!(read("").is_err());
    }

    // --- read_many ---

    #[test]
    fn read_many_single() {
        let vals = read_many("42").unwrap();
        assert_eq!(vals.len(), 1);
        assert_eq!(vals[0], Val::Int(42));
    }

    #[test]
    fn read_many_multiple() {
        let vals = read_many("(a) (b) (c)").unwrap();
        assert_eq!(vals.len(), 3);
    }

    #[test]
    fn read_many_empty() {
        let vals = read_many("").unwrap();
        assert!(vals.is_empty());
    }

    #[test]
    fn read_many_whitespace_only() {
        let vals = read_many("  ; just a comment\n  ").unwrap();
        assert!(vals.is_empty());
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
    fn display_bool() {
        assert_eq!(format!("{}", Val::Bool(true)), "true");
        assert_eq!(format!("{}", Val::Bool(false)), "false");
    }

    #[test]
    fn display_int() {
        assert_eq!(format!("{}", Val::Int(42)), "42");
        assert_eq!(format!("{}", Val::Int(-7)), "-7");
    }

    #[test]
    fn display_float() {
        assert_eq!(format!("{}", Val::Float(3.14)), "3.14");
        assert_eq!(format!("{}", Val::Float(1.0)), "1.0");
    }

    #[test]
    fn display_keyword() {
        assert_eq!(format!("{}", Val::Keyword("port".into())), ":port");
    }

    #[test]
    fn display_list() {
        let v = Val::List(vec![Val::Sym("host".into()), Val::Str("addr".into())]);
        assert_eq!(format!("{v}"), "(host \"addr\")");
    }

    #[test]
    fn display_empty_list() {
        assert_eq!(format!("{}", Val::List(vec![])), "()");
    }

    #[test]
    fn display_vector() {
        let v = Val::Vector(vec![Val::Int(1), Val::Int(2), Val::Int(3)]);
        assert_eq!(format!("{v}"), "[1 2 3]");
    }

    #[test]
    fn display_map() {
        let v = Val::Map(vec![
            (Val::Keyword("a".into()), Val::Int(1)),
            (Val::Keyword("b".into()), Val::Int(2)),
        ]);
        assert_eq!(format!("{v}"), "{:a 1 :b 2}");
    }

    #[test]
    fn display_set() {
        let v = Val::Set(vec![Val::Keyword("a".into()), Val::Keyword("b".into())]);
        assert_eq!(format!("{v}"), "#{:a :b}");
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

    #[test]
    fn roundtrip_vector() {
        let input = "[1 2 3]";
        let val = read(input).unwrap();
        assert_eq!(format!("{val}"), input);
    }

    #[test]
    fn roundtrip_map() {
        let input = "{:a 1 :b 2}";
        let val = read(input).unwrap();
        assert_eq!(format!("{val}"), input);
    }

    #[test]
    fn roundtrip_set() {
        let input = "#{:x :y}";
        let val = read(input).unwrap();
        assert_eq!(format!("{val}"), input);
    }

    #[test]
    fn roundtrip_keyword() {
        let input = ":my-key";
        let val = read(input).unwrap();
        assert_eq!(format!("{val}"), input);
    }

    #[test]
    fn roundtrip_bool() {
        assert_eq!(format!("{}", read("true").unwrap()), "true");
        assert_eq!(format!("{}", read("false").unwrap()), "false");
    }

    // --- session prefix resolution (ported from kernel) ---

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

    // --- init.d service declaration parsing ---

    /// Helper matching the kernel's `map_get_str` — extract a string value
    /// for a keyword key from a glia Map.
    fn map_get_str<'a>(pairs: &'a [(Val, Val)], key: &str) -> Option<&'a str> {
        pairs.iter().find_map(|(k, v)| match (k, v) {
            (Val::Keyword(k), Val::Str(s)) if k == key => Some(s.as_str()),
            _ => None,
        })
    }

    #[test]
    fn parse_initd_service_declaration() {
        // Exact format used by examples/chess/etc/init.d/chess.glia
        let input = r#"{:protocol  "chess"
 :handler   "bin/chess-handler.wasm"
 :namespace "wetware.chess.v1"}"#;

        let val = read(input).unwrap();
        let pairs = match &val {
            Val::Map(pairs) => pairs,
            other => panic!("expected Map, got {other:?}"),
        };

        assert_eq!(pairs.len(), 3);
        assert_eq!(map_get_str(pairs, "protocol"), Some("chess"));
        assert_eq!(
            map_get_str(pairs, "handler"),
            Some("bin/chess-handler.wasm")
        );
        assert_eq!(map_get_str(pairs, "namespace"), Some("wetware.chess.v1"));
    }

    #[test]
    fn initd_missing_key_returns_none() {
        let val = read(r#"{:protocol "chess"}"#).unwrap();
        let pairs = match &val {
            Val::Map(pairs) => pairs,
            other => panic!("expected Map, got {other:?}"),
        };
        assert_eq!(map_get_str(pairs, "protocol"), Some("chess"));
        assert_eq!(map_get_str(pairs, "handler"), None);
        assert_eq!(map_get_str(pairs, "namespace"), None);
    }

    #[test]
    fn initd_wrong_value_type_returns_none() {
        // :handler with a keyword value instead of a string
        let val = read(r#"{:protocol "chess" :handler :not-a-string}"#).unwrap();
        let pairs = match &val {
            Val::Map(pairs) => pairs,
            other => panic!("expected Map, got {other:?}"),
        };
        assert_eq!(map_get_str(pairs, "protocol"), Some("chess"));
        assert_eq!(map_get_str(pairs, "handler"), None);
    }
}
