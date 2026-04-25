//! Structured error values for Glia.
//!
//! Glia errors are values: `eval` returns `Result<Val, Val>`, and the error
//! type is itself a `Val`. This module establishes a canonical schema for
//! structured errors, so downstream consumers (the MCP adapter, AI agents
//! reading errors via `try`/`catch`, runtime introspection) get rich data
//! instead of opaque strings.
//!
//! # Schema
//!
//! Errors are `Val::Map`s with namespaced keyword keys:
//!
//! ```text
//! { :glia.error/type     <keyword>   ; tag identifying the variant
//!   :glia.error/message  <string>    ; human-readable summary
//!   :glia.error/hint     <string>    ; optional recovery suggestion
//!   ; ...variant-specific fields (e.g. :glia.error/symbol, :glia.error/function)
//! }
//! ```
//!
//! The namespaced keys (`glia.error/...`) avoid collision with user-defined
//! keys in error maps and follow Clojure's convention for library-owned
//! keywords.
//!
//! # Construction
//!
//! Use the typed constructor functions in this module (e.g. `unbound_symbol`,
//! `arity`, `type_mismatch`). Each constructor enforces the canonical schema
//! for its variant, so call sites can't malform the map shape.
//!
//! ```rust,ignore
//! return Err(glia::error::unbound_symbol("foo", Some("did you mean 'bar'?")));
//! ```
//!
//! Legacy `eval_err!` call sites that produce `Val::Str` errors continue to
//! work — accessor functions return `None` on plain-string errors, signaling
//! "unstructured legacy error" to consumers.
//!
//! # Inspection
//!
//! `data`, `message`, and `type_tag` mirror Clojure's `ex-data`/`ex-message`.
//! Plain-string errors return `None` from each, distinguishing structured
//! errors from legacy/foreign error values.
//!
//! # Future migration
//!
//! Transport currently uses `Result<Val, Val>` Err-propagation. A future
//! sprint will migrate to effect-based exceptions over the existing
//! `perform` / `with-effect-handler` machinery: `(throw err)` becomes
//! `(perform :glia.exception err)` that the handler doesn't resume; `try`/
//! `catch` becomes `with-effect-handler` matching on `:glia.error/type`.
//! The schema (this module) is transport-independent and survives unchanged.

use crate::{Val, ValMap};

// ----- Tag constants ------------------------------------------------------

/// Namespaced `:glia.error/type` keywords. The `tag::*` constants are the
/// single source of truth — call sites and consumers should never spell
/// these out as string literals.
pub mod tag {
    pub const PARSE: &str = "glia.error/parse";
    pub const UNBOUND_SYMBOL: &str = "glia.error/unbound-symbol";
    pub const ARITY: &str = "glia.error/arity-mismatch";
    pub const TYPE_MISMATCH: &str = "glia.error/type-mismatch";
    pub const CAP_CALL: &str = "glia.error/cap-call-failed";
    pub const RPC: &str = "glia.error/rpc-error";
    pub const EPOCH_EXPIRED: &str = "glia.error/epoch-expired";
    pub const PERMISSION_DENIED: &str = "glia.error/permission-denied";
    pub const FUEL_EXHAUSTED: &str = "glia.error/fuel-exhausted";
    pub const INTERNAL: &str = "glia.error/internal";
}

// ----- Schema-key constants -----------------------------------------------

/// Canonical `:glia.error/...` map keys. As with `tag::*`, these are the
/// single source of truth.
pub mod key {
    pub const TYPE: &str = "glia.error/type";
    pub const MESSAGE: &str = "glia.error/message";
    pub const HINT: &str = "glia.error/hint";
    pub const SYMBOL: &str = "glia.error/symbol";
    pub const FUNCTION: &str = "glia.error/function";
    pub const EXPECTED: &str = "glia.error/expected";
    pub const GOT: &str = "glia.error/got";
    pub const GOT_TYPE: &str = "glia.error/got-type";
    pub const CONTEXT: &str = "glia.error/context";
    pub const CAP: &str = "glia.error/cap";
    pub const METHOD: &str = "glia.error/method";
    pub const SOURCE_LOCATION: &str = "glia.error/source-location";
}

// ----- Internal helpers ---------------------------------------------------

#[inline]
fn kw(s: &str) -> Val {
    Val::Keyword(s.into())
}

/// Build the base error map with `:glia.error/type` and
/// `:glia.error/message` set. All variant constructors layer their own
/// fields on top of this.
fn base(tag: &'static str, message: String) -> Vec<(Val, Val)> {
    vec![
        (kw(key::TYPE), kw(tag)),
        (kw(key::MESSAGE), Val::Str(message)),
    ]
}

#[inline]
fn finalize(pairs: Vec<(Val, Val)>) -> Val {
    Val::Map(ValMap::from_pairs(pairs))
}

// ----- Variant constructors ----------------------------------------------

/// Parse error — input failed to tokenize or parse.
///
/// `location` is optional human-readable source location (e.g. `"foo.glia:42:7"`).
pub fn parse(location: Option<&str>, message: impl Into<String>) -> Val {
    let msg = message.into();
    let mut pairs = base(tag::PARSE, format!("parse error: {msg}"));
    if let Some(loc) = location {
        pairs.push((kw(key::SOURCE_LOCATION), Val::Str(loc.into())));
    }
    finalize(pairs)
}

/// Unbound symbol — reference to an undefined identifier.
///
/// `hint` is an optional recovery suggestion (e.g. `Some("did you mean 'bar'?")`).
pub fn unbound_symbol(symbol: &str, hint: Option<&str>) -> Val {
    let mut pairs = base(tag::UNBOUND_SYMBOL, format!("unbound symbol: {symbol}"));
    pairs.push((kw(key::SYMBOL), Val::Sym(symbol.into())));
    if let Some(h) = hint {
        pairs.push((kw(key::HINT), Val::Str(h.into())));
    }
    finalize(pairs)
}

/// Arity mismatch — wrong number of arguments to a function or special form.
///
/// `expected` is a human-readable arity description (e.g. `"2"`, `"2-3"`,
/// `"at least 1"`).
pub fn arity(function: &str, expected: &str, got: usize) -> Val {
    let mut pairs = base(
        tag::ARITY,
        format!("{function}: expected {expected} arg(s), got {got}"),
    );
    pairs.push((kw(key::FUNCTION), Val::Str(function.into())));
    pairs.push((kw(key::EXPECTED), Val::Str(expected.into())));
    pairs.push((kw(key::GOT), Val::Int(got as i64)));
    finalize(pairs)
}

/// Type mismatch — argument or operand of the wrong runtime type.
///
/// `context` is where the mismatch happened (e.g. `"def"`, `"if condition"`).
/// `expected` is a human-readable type description (e.g. `"symbol"`, `"number"`).
/// `got` is the offending value — its `Val` variant name is recorded.
pub fn type_mismatch(context: &str, expected: &str, got: &Val) -> Val {
    let got_type = val_type_name(got);
    let mut pairs = base(
        tag::TYPE_MISMATCH,
        format!("{context}: expected {expected}, got {got_type}"),
    );
    pairs.push((kw(key::CONTEXT), Val::Str(context.into())));
    pairs.push((kw(key::EXPECTED), Val::Str(expected.into())));
    pairs.push((kw(key::GOT_TYPE), Val::Str(got_type.into())));
    finalize(pairs)
}

/// Capability call failed — an RPC method on a capability returned an error.
pub fn cap_call(cap: &str, method: &str, message: impl Into<String>) -> Val {
    let msg = message.into();
    let mut pairs = base(tag::CAP_CALL, format!("{cap}.{method} failed: {msg}"));
    pairs.push((kw(key::CAP), Val::Str(cap.into())));
    pairs.push((kw(key::METHOD), Val::Str(method.into())));
    finalize(pairs)
}

/// Generic RPC failure (transport-level: disconnect, timeout, malformed
/// frame). Distinct from `cap_call`, which is a method-level failure with
/// a `cap`/`method` known to the caller.
pub fn rpc(message: impl Into<String>) -> Val {
    finalize(base(tag::RPC, format!("rpc error: {}", message.into())))
}

/// Epoch expired — a capability was used after the membrane epoch advanced.
pub fn epoch_expired(cap: &str) -> Val {
    let mut pairs = base(tag::EPOCH_EXPIRED, format!("epoch expired for cap '{cap}'"));
    pairs.push((kw(key::CAP), Val::Str(cap.into())));
    finalize(pairs)
}

/// Permission denied — access to a resource was refused (e.g. an
/// attenuated capability, a missing membrane grant).
pub fn permission_denied(what: &str, hint: Option<&str>) -> Val {
    let mut pairs = base(tag::PERMISSION_DENIED, format!("permission denied: {what}"));
    pairs.push((kw(key::CONTEXT), Val::Str(what.into())));
    if let Some(h) = hint {
        pairs.push((kw(key::HINT), Val::Str(h.into())));
    }
    finalize(pairs)
}

/// Fuel exhausted — the cell ran out of compute budget.
pub fn fuel_exhausted() -> Val {
    finalize(base(tag::FUEL_EXHAUSTED, "fuel exhausted".to_string()))
}

/// Internal error — invariant violation, "should not happen" condition.
/// **Not** a generic escape hatch; reach for a specific variant first.
/// Use only for genuine bugs that warrant attention in logs.
pub fn internal(context: &str, message: impl Into<String>) -> Val {
    let msg = message.into();
    let mut pairs = base(tag::INTERNAL, format!("internal error in {context}: {msg}"));
    pairs.push((kw(key::CONTEXT), Val::Str(context.into())));
    finalize(pairs)
}

// ----- Inspection accessors ----------------------------------------------

/// Extract the structured error map from an error `Val`.
///
/// Mirrors Clojure's `ex-data`. Returns `None` for plain-string errors
/// (legacy `eval_err!` call sites) or non-map values, signaling the error
/// is unstructured.
pub fn data(err: &Val) -> Option<&ValMap> {
    if let Val::Map(m) = err {
        // Only treat as structured if `:glia.error/type` is present.
        if m.contains_key(&kw(key::TYPE)) {
            return Some(m);
        }
    }
    None
}

/// Extract the error message.
///
/// Mirrors Clojure's `ex-message`. Returns the `:glia.error/message` field
/// from a structured error, the contents of a `Val::Str` for legacy
/// errors, or `None` otherwise.
pub fn message(err: &Val) -> Option<&str> {
    if let Val::Str(s) = err {
        return Some(s.as_str());
    }
    let m = data(err)?;
    match m.get(&kw(key::MESSAGE)) {
        Some(Val::Str(s)) => Some(s.as_str()),
        _ => None,
    }
}

/// Extract the namespaced `:glia.error/type` tag.
///
/// Returns `None` for plain-string errors or maps without a `:glia.error/type`
/// field. Use `tag::*` constants to compare against, never string literals.
pub fn type_tag(err: &Val) -> Option<&str> {
    let m = data(err)?;
    match m.get(&kw(key::TYPE)) {
        Some(Val::Keyword(k)) => Some(k.as_str()),
        _ => None,
    }
}

/// Extract the optional `:glia.error/hint` field.
pub fn hint(err: &Val) -> Option<&str> {
    let m = data(err)?;
    match m.get(&kw(key::HINT)) {
        Some(Val::Str(s)) => Some(s.as_str()),
        _ => None,
    }
}

// ----- Internal helpers ---------------------------------------------------

/// Human-readable name for a `Val` variant — used by `type_mismatch` to
/// fill the `:glia.error/got-type` field.
pub(crate) fn val_type_name(v: &Val) -> &'static str {
    match v {
        Val::Nil => "nil",
        Val::Bool(_) => "bool",
        Val::Int(_) => "int",
        Val::Float(_) => "float",
        Val::Str(_) => "string",
        Val::Sym(_) => "symbol",
        Val::Keyword(_) => "keyword",
        Val::List(_) => "list",
        Val::Vector(_) => "vector",
        Val::Map(_) => "map",
        Val::Set(_) => "set",
        Val::Bytes(_) => "bytes",
        Val::Fn { .. } => "fn",
        Val::Recur(_) => "recur",
        Val::Macro { .. } => "macro",
        Val::Effect { .. } => "effect",
        Val::NativeFn { .. } => "native-fn",
        Val::AsyncNativeFn { .. } => "async-native-fn",
        Val::Resume(_) => "resume",
        Val::Cap { .. } => "cap",
        Val::Cell { .. } => "cell",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn val_str(s: &str) -> Val {
        Val::Str(s.into())
    }

    // ----- Schema correctness for each variant ---------------------------

    #[test]
    fn parse_has_canonical_shape() {
        let err = parse(Some("foo.glia:1:1"), "unexpected token");
        assert_eq!(type_tag(&err), Some(tag::PARSE));
        assert!(message(&err).unwrap().contains("unexpected token"));
        let m = data(&err).unwrap();
        assert!(matches!(
            m.get(&kw(key::SOURCE_LOCATION)),
            Some(Val::Str(loc)) if loc == "foo.glia:1:1"
        ));
    }

    #[test]
    fn parse_omits_location_when_none() {
        let err = parse(None, "syntax error");
        let m = data(&err).unwrap();
        assert!(m.get(&kw(key::SOURCE_LOCATION)).is_none());
    }

    #[test]
    fn unbound_symbol_includes_symbol_field() {
        let err = unbound_symbol("foo", Some("did you mean 'bar'?"));
        assert_eq!(type_tag(&err), Some(tag::UNBOUND_SYMBOL));
        let m = data(&err).unwrap();
        assert!(matches!(m.get(&kw(key::SYMBOL)), Some(Val::Sym(s)) if s == "foo"));
        assert_eq!(hint(&err), Some("did you mean 'bar'?"));
    }

    #[test]
    fn unbound_symbol_omits_hint_when_none() {
        let err = unbound_symbol("foo", None);
        assert!(hint(&err).is_none());
    }

    #[test]
    fn arity_records_function_expected_got() {
        let err = arity("def", "1-2", 5);
        assert_eq!(type_tag(&err), Some(tag::ARITY));
        let m = data(&err).unwrap();
        assert!(matches!(m.get(&kw(key::FUNCTION)), Some(Val::Str(s)) if s == "def"));
        assert!(matches!(m.get(&kw(key::EXPECTED)), Some(Val::Str(s)) if s == "1-2"));
        assert!(matches!(m.get(&kw(key::GOT)), Some(Val::Int(5))));
    }

    #[test]
    fn type_mismatch_records_got_type_from_val() {
        let err = type_mismatch("if", "bool", &Val::Int(42));
        assert_eq!(type_tag(&err), Some(tag::TYPE_MISMATCH));
        let m = data(&err).unwrap();
        assert!(matches!(m.get(&kw(key::CONTEXT)), Some(Val::Str(s)) if s == "if"));
        assert!(matches!(m.get(&kw(key::EXPECTED)), Some(Val::Str(s)) if s == "bool"));
        assert!(matches!(m.get(&kw(key::GOT_TYPE)), Some(Val::Str(s)) if s == "int"));
    }

    #[test]
    fn cap_call_records_cap_and_method() {
        let err = cap_call("host", "listen", "stale epoch");
        assert_eq!(type_tag(&err), Some(tag::CAP_CALL));
        assert!(message(&err).unwrap().contains("host.listen"));
        let m = data(&err).unwrap();
        assert!(matches!(m.get(&kw(key::CAP)), Some(Val::Str(s)) if s == "host"));
        assert!(matches!(m.get(&kw(key::METHOD)), Some(Val::Str(s)) if s == "listen"));
    }

    #[test]
    fn rpc_uses_namespaced_tag() {
        let err = rpc("connection reset");
        assert_eq!(type_tag(&err), Some(tag::RPC));
        assert!(message(&err).unwrap().contains("connection reset"));
    }

    #[test]
    fn epoch_expired_records_cap() {
        let err = epoch_expired("routing");
        assert_eq!(type_tag(&err), Some(tag::EPOCH_EXPIRED));
        let m = data(&err).unwrap();
        assert!(matches!(m.get(&kw(key::CAP)), Some(Val::Str(s)) if s == "routing"));
    }

    #[test]
    fn permission_denied_records_context_and_hint() {
        let err = permission_denied("network/dial", Some("graft network cap to enable"));
        assert_eq!(type_tag(&err), Some(tag::PERMISSION_DENIED));
        let m = data(&err).unwrap();
        assert!(matches!(
            m.get(&kw(key::CONTEXT)),
            Some(Val::Str(s)) if s == "network/dial"
        ));
        assert_eq!(hint(&err), Some("graft network cap to enable"));
    }

    #[test]
    fn fuel_exhausted_has_correct_tag() {
        let err = fuel_exhausted();
        assert_eq!(type_tag(&err), Some(tag::FUEL_EXHAUSTED));
    }

    #[test]
    fn internal_records_context() {
        let err = internal("eval_fn_body", "unreachable variant");
        assert_eq!(type_tag(&err), Some(tag::INTERNAL));
        assert!(message(&err).unwrap().contains("unreachable variant"));
    }

    // ----- Inspection accessors ------------------------------------------

    #[test]
    fn data_returns_none_for_plain_string() {
        assert!(data(&val_str("legacy error")).is_none());
    }

    #[test]
    fn data_returns_none_for_non_error_map() {
        let m = ValMap::from_pairs(vec![(kw("foo"), Val::Int(1))]);
        let val = Val::Map(m);
        // Map without :glia.error/type isn't an error.
        assert!(data(&val).is_none());
    }

    #[test]
    fn data_returns_none_for_other_val_types() {
        assert!(data(&Val::Nil).is_none());
        assert!(data(&Val::Int(42)).is_none());
        assert!(data(&Val::Sym("foo".into())).is_none());
    }

    #[test]
    fn message_falls_back_to_plain_string() {
        // Legacy plain-string errors still produce a message.
        assert_eq!(message(&val_str("boom")), Some("boom"));
    }

    #[test]
    fn message_extracts_from_structured_error() {
        let err = unbound_symbol("foo", None);
        assert!(message(&err).unwrap().contains("unbound symbol: foo"));
    }

    #[test]
    fn message_returns_none_for_non_string_non_error() {
        assert!(message(&Val::Int(42)).is_none());
    }

    #[test]
    fn type_tag_returns_none_for_plain_string() {
        assert!(type_tag(&val_str("oops")).is_none());
    }

    #[test]
    fn hint_returns_none_when_absent() {
        let err = unbound_symbol("foo", None);
        assert!(hint(&err).is_none());
    }

    // ----- val_type_name -------------------------------------------------

    #[test]
    fn val_type_name_covers_all_variants() {
        // Sanity check on the helper: a few common cases.
        assert_eq!(val_type_name(&Val::Nil), "nil");
        assert_eq!(val_type_name(&Val::Int(0)), "int");
        assert_eq!(val_type_name(&Val::Str("".into())), "string");
        assert_eq!(val_type_name(&Val::Keyword("k".into())), "keyword");
        assert_eq!(val_type_name(&Val::Map(ValMap::new())), "map");
    }
}
