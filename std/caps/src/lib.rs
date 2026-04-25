//! Shared Glia effect handler factories for Cap'n Proto capabilities.
//!
//! Extracted from the shell cell so that multiple cells (shell, MCP, etc.)
//! can reuse the same handler implementations without duplication.

use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::rc::Rc;

use glia::Val;

// Re-export extract_method from glia for downstream consumers (shell, MCP).
pub use glia::extract_method;

// Re-export schema_capnp so generated stem code can resolve `crate::schema_capnp`.
pub use capnp::schema_capnp;

// Generated Cap'n Proto modules.
#[allow(dead_code)]
pub mod system_capnp {
    include!(concat!(env!("OUT_DIR"), "/system_capnp.rs"));
}
#[allow(dead_code)]
pub mod stem_capnp {
    include!(concat!(env!("OUT_DIR"), "/stem_capnp.rs"));
}
#[allow(dead_code)]
pub mod routing_capnp {
    include!(concat!(env!("OUT_DIR"), "/routing_capnp.rs"));
}
#[allow(dead_code)]
pub mod http_capnp {
    include!(concat!(env!("OUT_DIR"), "/http_capnp.rs"));
}

// ---------------------------------------------------------------------------
// File loading (same pattern as kernel)
// ---------------------------------------------------------------------------

thread_local! {
    static LOAD_CACHE: RefCell<HashMap<String, Vec<u8>>> = RefCell::new(HashMap::new());
}

pub fn eval_load(args: &[Val]) -> Result<Val, Val> {
    let path = match args.first() {
        Some(Val::Str(s)) => s.clone(),
        Some(Val::Sym(s)) => s.clone(),
        _ => return Err(Val::from("load: expected a path string")),
    };

    // Normalize: ensure leading /
    let resolved = if path.starts_with('/') {
        path.clone()
    } else {
        let root = std::env::var("WW_ROOT").unwrap_or_default();
        format!("{root}/{path}")
    };

    LOAD_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(bytes) = cache.get(&resolved) {
            return Ok(Val::Bytes(bytes.clone()));
        }
        match std::fs::read(&resolved) {
            Ok(bytes) => {
                cache.insert(resolved, bytes.clone());
                Ok(Val::Bytes(bytes))
            }
            Err(e) => Err(Val::from(format!("load: {resolved}: {e}"))),
        }
    })
}

// ---------------------------------------------------------------------------
// Cap handler helpers
// ---------------------------------------------------------------------------

pub fn call_resume(resume: &Val, val: Val) -> Result<Val, Val> {
    match resume {
        Val::NativeFn { func, .. } => func(&[val]),
        _ => Err(Val::from("cap handler: invalid resume function")),
    }
}


// ---------------------------------------------------------------------------
// Import handler — module loading as a membrane capability
// ---------------------------------------------------------------------------

/// Schema CID for the import capability (local, not a real capnp schema).
pub const IMPORT_SCHEMA_CID: &str = "local:import";

thread_local! {
    /// Cache for imported module maps, keyed by resolved path.
    /// Second `(def m (perform import "core"))` returns the cached map.
    static IMPORT_CACHE: RefCell<HashMap<String, Val>> = RefCell::new(HashMap::new());
}

/// Create a `Val::Cap` representing the import capability.
///
/// Callers bind this in the env so `(perform import "core")` resolves
/// `import` to a cap value that the effect system can match.
pub fn make_import_cap() -> Val {
    Val::Cap {
        name: "import".into(),
        schema_cid: IMPORT_SCHEMA_CID.into(),
        inner: Rc::new(()),
    }
}

/// Clear the import cache. Useful for testing or when the virtual filesystem changes.
pub fn clear_import_cache() {
    IMPORT_CACHE.with(|cache| cache.borrow_mut().clear());
}

/// Resolve an import path to an absolute filesystem path.
///
/// - Relative path (no leading `/`): resolved to `/lib/{path}.glia`
/// - Absolute path (leading `/`): appended with `.glia`
fn resolve_import_path(path: &str) -> String {
    if path.starts_with('/') {
        format!("{}.glia", path)
    } else {
        format!("/lib/{}.glia", path)
    }
}

/// Create the import effect handler.
///
/// Usage: `(def core (perform import "core"))`
///
/// The handler:
/// 1. Resolves the path (relative → `/lib/`, absolute → as-is)
/// 2. Checks the import cache
/// 3. If not cached: loads file, parses as Glia forms, evals in fresh scope
/// 4. Collects bindings as a `Val::Map`
/// 5. Caches the map for idempotent re-import
/// 6. Resumes with the map
///
/// The caller binds the returned map: `(def core (perform import "core"))`.
/// Access members via the map: `(core :help)`.
pub fn make_import_handler() -> Val {
    Val::AsyncNativeFn {
        name: "import-handler".into(),
        func: Rc::new(move |args: Vec<Val>| {
            Box::pin(async move {
                // Effect data for `(perform import "core")`:
                // args[0] = data list, args[1] = resume continuation.
                //
                // Cap-targeted perform packs all args after the target into a
                // list: `(perform import "core")` → data = Val::List(["core"]).
                let data = &args[0];
                let resume = &args[1];

                // Extract the path from the data list.
                // `(perform import "core")` → data = ["core"]
                let path = match data {
                    Val::List(items) => match items.first() {
                        Some(Val::Str(s)) => s.clone(),
                        other => {
                            let desc = match other {
                                Some(v) => format!("{v}"),
                                None => "nothing".into(),
                            };
                            return Err(Val::from(format!(
                                "import: expected string path, got {desc}"
                            )));
                        }
                    },
                    Val::Str(s) => s.clone(),
                    other => {
                        return Err(Val::from(format!(
                            "import: expected path string, got {other}"
                        )));
                    }
                };

                let resolved = resolve_import_path(&path);

                // Check cache
                let cached =
                    IMPORT_CACHE.with(|cache| cache.borrow().get(&resolved).cloned());
                if let Some(map) = cached {
                    return call_resume(resume, map);
                }

                // Load the file via eval_load
                let bytes_val = eval_load(&[Val::Str(resolved.clone())])?;
                let content = match &bytes_val {
                    Val::Bytes(b) => std::str::from_utf8(b)
                        .map_err(|e| {
                            Val::from(format!("import: invalid UTF-8 in {resolved}: {e}"))
                        })?
                        .to_string(),
                    Val::Str(s) => s.clone(),
                    other => {
                        return Err(Val::from(format!(
                            "import: load returned {other}, expected bytes or string"
                        )));
                    }
                };

                // Parse the file as Glia forms
                let forms = glia::read_many(&content)
                    .map_err(|e| Val::from(format!("import: parse error in {resolved}: {e}")))?;

                // Evaluate in a fresh Env (isolated scope)
                let mut import_env = glia::eval::Env::new();
                // Load prelude so imported modules can use `defn`, `when`, etc.
                {
                    let prelude_forms = glia::read_many(glia::PRELUDE)
                        .map_err(|e| Val::from(format!("import: prelude parse: {e}")))?;
                    struct NoopDispatch;
                    impl glia::eval::Dispatch for NoopDispatch {
                        fn call<'a>(
                            &'a self,
                            name: &'a str,
                            _args: &'a [glia::Val],
                        ) -> std::pin::Pin<
                            Box<
                                dyn std::future::Future<Output = Result<glia::Val, glia::Val>>
                                    + 'a,
                            >,
                        > {
                            Box::pin(std::future::ready(Err(glia::Val::from(format!(
                                "{name}: not available during import"
                            )))))
                        }
                    }
                    let noop = NoopDispatch;
                    for form in &prelude_forms {
                        // Prelude forms are synchronous (macros only), so poll once.
                        let mut fut =
                            Box::pin(glia::eval::eval_toplevel(form, &mut import_env, &noop));
                        let waker = std::task::Waker::noop();
                        let mut cx = std::task::Context::from_waker(&waker);
                        match fut.as_mut().poll(&mut cx) {
                            std::task::Poll::Ready(Ok(_)) => {}
                            std::task::Poll::Ready(Err(e)) => {
                                return Err(Val::from(format!("import: prelude error: {e}")));
                            }
                            std::task::Poll::Pending => {
                                return Err(Val::from("import: prelude unexpectedly pending"));
                            }
                        }
                    }
                }

                // Evaluate module forms
                {
                    struct NoopDispatch;
                    impl glia::eval::Dispatch for NoopDispatch {
                        fn call<'a>(
                            &'a self,
                            name: &'a str,
                            _args: &'a [glia::Val],
                        ) -> std::pin::Pin<
                            Box<
                                dyn std::future::Future<Output = Result<glia::Val, glia::Val>>
                                    + 'a,
                            >,
                        > {
                            Box::pin(std::future::ready(Err(glia::Val::from(format!(
                                "{name}: not available during import"
                            )))))
                        }
                    }
                    let noop = NoopDispatch;
                    for form in &forms {
                        let analyzed = glia::expr::analyze(form)
                            .map_err(|e| Val::from(format!("import: analyze error in {resolved}: {e}")))?;
                        let mut fut = Box::pin(glia::eval::eval_toplevel_expr(
                            &analyzed,
                            &mut import_env,
                            &noop,
                        ));
                        let waker = std::task::Waker::noop();
                        let mut cx = std::task::Context::from_waker(&waker);
                        match fut.as_mut().poll(&mut cx) {
                            std::task::Poll::Ready(Ok(_)) => {}
                            std::task::Poll::Ready(Err(e)) => {
                                return Err(Val::from(format!("import: eval error in {resolved}: {e}")));
                            }
                            std::task::Poll::Pending => {
                                return Err(Val::from(format!(
                                    "import: eval unexpectedly pending in {resolved}"
                                )));
                            }
                        }
                    }
                }

                // Collect bindings as a Val::Map
                let bindings = import_env.bindings();
                let map_entries: Vec<(Val, Val)> = bindings
                    .into_iter()
                    .map(|(name, val)| (Val::Keyword(name), val))
                    .collect();
                let module_map = Val::Map(glia::ValMap::from_pairs(map_entries));

                // Cache the map
                IMPORT_CACHE.with(|cache| {
                    cache.borrow_mut().insert(resolved, module_map.clone());
                });

                call_resume(resume, module_map)
            })
        }),
    }
}

// ---------------------------------------------------------------------------
// Effect handler factories
// ---------------------------------------------------------------------------

pub fn make_host_handler(host: system_capnp::host::Client) -> Val {
    Val::AsyncNativeFn {
        name: "host-handler".into(),
        func: Rc::new(move |args: Vec<Val>| {
            let host = host.clone();
            Box::pin(async move {
                let (method, _rest) = extract_method(&args[0])?;
                let resume = &args[1];

                match method {
                    "id" => {
                        let resp = host
                            .id_request()
                            .send()
                            .promise
                            .await
                            .map_err(|e| Val::from(e.to_string()))?;
                        let peer_id_bytes = resp
                            .get()
                            .map_err(|e| Val::from(e.to_string()))?
                            .get_peer_id()
                            .map_err(|e| Val::from(e.to_string()))?;
                        let encoded = bs58::encode(peer_id_bytes).into_string();
                        call_resume(resume, Val::Str(encoded))
                    }
                    "addrs" => {
                        let resp = host
                            .addrs_request()
                            .send()
                            .promise
                            .await
                            .map_err(|e| Val::from(e.to_string()))?;
                        let addrs = resp
                            .get()
                            .map_err(|e| Val::from(e.to_string()))?
                            .get_addrs()
                            .map_err(|e| Val::from(e.to_string()))?;
                        let items: Vec<Val> = addrs
                            .iter()
                            .filter_map(|a| {
                                a.ok().and_then(|bytes| {
                                    multiaddr::Multiaddr::try_from(bytes.to_vec())
                                        .ok()
                                        .map(|ma| Val::Str(ma.to_string()))
                                })
                            })
                            .collect();
                        call_resume(resume, Val::List(items))
                    }
                    "peers" => {
                        let resp = host
                            .peers_request()
                            .send()
                            .promise
                            .await
                            .map_err(|e| Val::from(e.to_string()))?;
                        let peers = resp
                            .get()
                            .map_err(|e| Val::from(e.to_string()))?
                            .get_peers()
                            .map_err(|e| Val::from(e.to_string()))?;
                        let items: Vec<Val> = peers
                            .iter()
                            .filter_map(|p| {
                                let peer_id = p.get_peer_id().ok()?;
                                let encoded = bs58::encode(peer_id).into_string();
                                let addrs: Vec<Val> = p
                                    .get_addrs()
                                    .ok()?
                                    .iter()
                                    .filter_map(|a| {
                                        a.ok().and_then(|bytes| {
                                            multiaddr::Multiaddr::try_from(bytes.to_vec())
                                                .ok()
                                                .map(|ma| Val::Str(ma.to_string()))
                                        })
                                    })
                                    .collect();
                                Some(Val::Map(glia::ValMap::from_pairs(vec![
                                    (Val::Keyword("peer-id".into()), Val::Str(encoded)),
                                    (Val::Keyword("addrs".into()), Val::List(addrs)),
                                ])))
                            })
                            .collect();
                        call_resume(resume, Val::List(items))
                    }
                    other => Err(Val::from(format!("host: unknown method :{other}"))),
                }
            })
        }),
    }
}

pub fn make_routing_handler(routing: routing_capnp::routing::Client) -> Val {
    Val::AsyncNativeFn {
        name: "routing-handler".into(),
        func: Rc::new(move |args: Vec<Val>| {
            let routing = routing.clone();
            Box::pin(async move {
                let (method, rest) = extract_method(&args[0])?;
                let resume = &args[1];

                match method {
                    "provide" => {
                        let key = match rest.first() {
                            Some(Val::Str(s)) => s.clone(),
                            _ => return Err(Val::from("routing :provide — expected key string")),
                        };
                        let mut req = routing.provide_request();
                        req.get().set_key(&key);
                        req.send()
                            .promise
                            .await
                            .map_err(|e| Val::from(e.to_string()))?;
                        call_resume(resume, Val::Nil)
                    }
                    "hash" => {
                        let data = match rest.first() {
                            Some(Val::Str(s)) => s.as_bytes().to_vec(),
                            Some(Val::Bytes(b)) => b.clone(),
                            _ => return Err(Val::from("routing :hash — expected string or bytes")),
                        };
                        let mut req = routing.hash_request();
                        req.get().set_data(&data);
                        let resp = req
                            .send()
                            .promise
                            .await
                            .map_err(|e| Val::from(e.to_string()))?;
                        let key = resp
                            .get()
                            .map_err(|e| Val::from(e.to_string()))?
                            .get_key()
                            .map_err(|e| Val::from(e.to_string()))?
                            .to_string()
                            .map_err(|e| Val::from(format!("{e}")))?;
                        call_resume(resume, Val::Str(key))
                    }
                    // findProviders uses streaming sink — deferred to follow-up.
                    other => Err(Val::from(format!("routing: unknown method :{other}"))),
                }
            })
        }),
    }
}

/// Resolve a Glia-supplied path against the cell's root.
///
/// `/ipfs/<cid>` and `/ipns/<key>` paths pass through unchanged — the
/// host's CidTree-backed VFS handles content addressing. Absolute paths
/// also pass through. Relative paths are resolved against `$WW_ROOT`,
/// falling back to `/` when unset.
fn resolve_fs_path(path: &str) -> String {
    if path.starts_with('/') {
        return path.to_string();
    }
    let root = std::env::var("WW_ROOT").unwrap_or_else(|_| "/".to_string());
    if root.ends_with('/') {
        format!("{root}{path}")
    } else {
        format!("{root}/{path}")
    }
}

/// Map a `std::fs::FileType` to a Glia keyword.
fn file_type_keyword(ft: std::fs::FileType) -> Val {
    if ft.is_dir() {
        Val::Keyword("dir".into())
    } else if ft.is_symlink() {
        Val::Keyword("symlink".into())
    } else {
        Val::Keyword("file".into())
    }
}

/// Build a `Val::Map` describing a single filesystem entry.
fn entry_map(name: String, size: u64, ft: std::fs::FileType) -> Val {
    Val::Map(glia::ValMap::from_pairs(vec![
        (Val::Keyword("name".into()), Val::Str(name)),
        (Val::Keyword("size".into()), Val::Int(size as i64)),
        (Val::Keyword("type".into()), file_type_keyword(ft)),
    ]))
}

/// Glia effect handler for the `fs` capability — content-addressed
/// filesystem read access via the WASI VFS (CidTree-backed at runtime).
///
/// Methods:
///   - `:read path`     → `Val::Bytes`  (raw file contents)
///   - `:read-str path` → `Val::Str`    (UTF-8 decoded; structured error on invalid UTF-8)
///   - `:ls path`       → `Val::List`   (entries `{:name :size :type}` where type ∈ #{:file :dir :symlink})
///   - `:stat path`     → `Val::Map`    (single-entry map: `{:size :type}`)
///   - `:exists? path`  → `Val::Bool`   (false on missing path; never errors)
///
/// Path resolution (see `resolve_fs_path`):
///   - Absolute paths (incl. `/ipfs/...`, `/ipns/...`) pass through to
///     the WASI VFS
///   - Relative paths are resolved against `$WW_ROOT` (falls back to `/`)
///
/// Replaces the prior `make_ipfs_handler` (which exposed `:cat`/`:ls`
/// under the `ipfs` cap name). Cap-name change `ipfs` → `fs` is part of
/// the same cleanup: post-#415/#416, content access is uniformly via
/// the content-addressed VFS, not a separate IPFS surface.
pub fn make_fs_handler() -> Val {
    Val::AsyncNativeFn {
        name: "fs-handler".into(),
        func: Rc::new(move |args: Vec<Val>| {
            Box::pin(async move {
                let (method, rest) = extract_method(&args[0])?;
                let resume = &args[1];

                let path_arg = |method_label: &str| -> Result<String, Val> {
                    match rest.first() {
                        Some(Val::Str(s)) => Ok(s.clone()),
                        Some(other) => Err(glia::error::type_mismatch(
                            &format!("fs :{method_label}"),
                            "path string",
                            other,
                        )),
                        None => Err(glia::error::arity(
                            &format!("fs :{method_label}"),
                            "1",
                            rest.len(),
                        )),
                    }
                };

                match method {
                    "read" => {
                        let path = path_arg("read")?;
                        let resolved = resolve_fs_path(&path);
                        match std::fs::read(&resolved) {
                            Ok(bytes) => call_resume(resume, Val::Bytes(bytes)),
                            Err(e) => Err(glia::error::cap_call(
                                "fs",
                                "read",
                                format!("{resolved}: {e}"),
                            )),
                        }
                    }
                    "read-str" => {
                        let path = path_arg("read-str")?;
                        let resolved = resolve_fs_path(&path);
                        let bytes = std::fs::read(&resolved).map_err(|e| {
                            glia::error::cap_call("fs", "read-str", format!("{resolved}: {e}"))
                        })?;
                        match String::from_utf8(bytes) {
                            Ok(s) => call_resume(resume, Val::Str(s)),
                            Err(e) => Err(glia::error::cap_call(
                                "fs",
                                "read-str",
                                format!("{resolved}: invalid UTF-8: {e}"),
                            )),
                        }
                    }
                    "ls" => {
                        let path = path_arg("ls")?;
                        let resolved = resolve_fs_path(&path);
                        match std::fs::read_dir(&resolved) {
                            Ok(entries) => {
                                let items: Vec<Val> = entries
                                    .filter_map(|e| {
                                        let e = e.ok()?;
                                        let meta = e.metadata().ok()?;
                                        let name =
                                            e.file_name().to_string_lossy().to_string();
                                        Some(entry_map(name, meta.len(), meta.file_type()))
                                    })
                                    .collect();
                                call_resume(resume, Val::List(items))
                            }
                            Err(e) => Err(glia::error::cap_call(
                                "fs",
                                "ls",
                                format!("{resolved}: {e}"),
                            )),
                        }
                    }
                    "stat" => {
                        let path = path_arg("stat")?;
                        let resolved = resolve_fs_path(&path);
                        match std::fs::metadata(&resolved) {
                            Ok(meta) => {
                                let m = Val::Map(glia::ValMap::from_pairs(vec![
                                    (Val::Keyword("size".into()), Val::Int(meta.len() as i64)),
                                    (Val::Keyword("type".into()), file_type_keyword(meta.file_type())),
                                ]));
                                call_resume(resume, m)
                            }
                            Err(e) => Err(glia::error::cap_call(
                                "fs",
                                "stat",
                                format!("{resolved}: {e}"),
                            )),
                        }
                    }
                    "exists?" => {
                        let path = path_arg("exists?")?;
                        let resolved = resolve_fs_path(&path);
                        // exists? never errors — missing path is just `false`.
                        let exists = std::fs::metadata(&resolved).is_ok();
                        call_resume(resume, Val::Bool(exists))
                    }
                    other => Err(glia::error::cap_call(
                        "fs",
                        other,
                        format!("unknown method :{other}"),
                    )),
                }
            })
        }),
    }
}

// ---------------------------------------------------------------------------
// Effect handler wrapping — nests with-effect-handler forms around an expr
// ---------------------------------------------------------------------------

/// Wraps a Glia form in standard capability effect handlers.
///
/// The `extra_caps` parameter allows callers to inject additional
/// capability names (e.g. "auction") that sit outside the core set.
pub fn wrap_with_handlers(form: &Val, extra_caps: &[&str]) -> Val {
    // Innermost: :load effect handler
    let with_load = Val::List(vec![
        Val::Sym("with-effect-handler".into()),
        Val::Keyword("load".into()),
        Val::List(vec![
            Val::Sym("fn".into()),
            Val::Vector(vec![Val::Sym("path".into()), Val::Sym("resume".into())]),
            Val::List(vec![
                Val::Sym("resume".into()),
                Val::List(vec![Val::Sym("load".into()), Val::Sym("path".into())]),
            ]),
        ]),
        form.clone(),
    ]);

    // Wrap in cap handlers (innermost to outermost)
    // Core caps first, then any extras
    let mut caps: Vec<&str> = vec!["import", "routing", "fs", "host"];
    for extra in extra_caps {
        caps.insert(0, extra);
    }
    let mut wrapped = with_load;
    for cap_name in &caps {
        let handler_name = format!("{cap_name}-handler");
        wrapped = Val::List(vec![
            Val::Sym("with-effect-handler".into()),
            Val::Sym(cap_name.to_string()),
            Val::Sym(handler_name),
            wrapped,
        ]);
    }
    wrapped
}

// ---------------------------------------------------------------------------
// Graft helpers: name-based lookup in parallel lists
// ---------------------------------------------------------------------------

/// Look up a typed capability by name from the graft caps list.
pub fn get_graft_cap<T: capnp::capability::FromClientHook>(
    caps: &capnp::struct_list::Reader<'_, stem_capnp::export::Owned>,
    name: &str,
) -> Result<T, capnp::Error> {
    for i in 0..caps.len() {
        let entry = caps.get(i);
        let n = entry.get_name()?.to_str().map_err(|e| capnp::Error::failed(e.to_string()))?;
        if n == name {
            return entry.get_cap().get_as_capability();
        }
    }
    Err(capnp::Error::failed(format!(
        "capability '{name}' not found in graft response"
    )))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_path_resolution_relative() {
        assert_eq!(resolve_import_path("core"), "/lib/core.glia");
    }

    #[test]
    fn import_path_resolution_nested() {
        assert_eq!(resolve_import_path("std/net"), "/lib/std/net.glia");
    }

    #[test]
    fn import_path_resolution_absolute() {
        assert_eq!(resolve_import_path("/absolute/path"), "/absolute/path.glia");
    }

    #[test]
    fn import_cache_clear() {
        IMPORT_CACHE.with(|cache| {
            cache
                .borrow_mut()
                .insert("/lib/test.glia".into(), Val::Map(glia::ValMap::new()));
        });
        clear_import_cache();
        IMPORT_CACHE.with(|cache| {
            assert!(cache.borrow().is_empty());
        });
    }

    #[test]
    fn import_handler_returns_map() {
        clear_import_cache();

        // Pre-populate the cache to test that the handler returns a map
        let test_map = Val::Map(glia::ValMap::from_pairs(vec![
            (Val::Keyword("x".into()), Val::Int(42)),
            (Val::Keyword("y".into()), Val::Int(99)),
        ]));
        IMPORT_CACHE.with(|cache| {
            cache
                .borrow_mut()
                .insert("/lib/core.glia".into(), test_map.clone());
        });

        // Build the handler and call it with cached data
        let handler = make_import_handler();
        match &handler {
            Val::AsyncNativeFn { func, .. } => {
                // Simulate: (perform import "core")
                // Effect data is a list of the args: ["core"]
                let data = Val::List(vec![Val::Str("core".into())]);

                // Build a resume function that captures the result
                let result = Rc::new(RefCell::new(None));
                let result_clone = result.clone();
                let resume = Val::NativeFn {
                    name: "test-resume".into(),
                    func: Rc::new(move |args: &[Val]| {
                        *result_clone.borrow_mut() = Some(args[0].clone());
                        Ok(Val::Nil)
                    }),
                };

                let args = vec![data, resume];
                let fut = func(args);

                // Poll the future (should resolve immediately for cached imports)
                let waker = std::task::Waker::noop();
                let mut cx = std::task::Context::from_waker(&waker);
                let mut pinned = fut;
                match std::pin::Pin::new(&mut pinned).poll(&mut cx) {
                    std::task::Poll::Ready(Ok(_)) => {}
                    std::task::Poll::Ready(Err(e)) => panic!("import handler failed: {e}"),
                    std::task::Poll::Pending => panic!("import handler unexpectedly pending"),
                }

                // Verify the result is the cached map
                let r = result.borrow();
                let result_val = r.as_ref().expect("resume should have been called");
                match result_val {
                    Val::Map(entries) => {
                        assert_eq!(entries.len(), 2);
                        // Check that :x is 42
                        let x = entries
                            .iter()
                            .find(|(k, _)| matches!(k, Val::Keyword(s) if s == "x"))
                            .map(|(_, v)| v);
                        assert_eq!(x, Some(&Val::Int(42)));
                    }
                    other => panic!("expected Map, got {other}"),
                }
            }
            _ => panic!("expected AsyncNativeFn"),
        }

        clear_import_cache();
    }

    #[test]
    fn import_cached_returns_same_map() {
        clear_import_cache();

        let test_map = Val::Map(glia::ValMap::from_pairs(vec![(Val::Keyword("a".into()), Val::Int(1))]));
        IMPORT_CACHE.with(|cache| {
            cache
                .borrow_mut()
                .insert("/lib/cached.glia".into(), test_map.clone());
        });

        // Call handler twice — both should return the same cached map
        let handler = make_import_handler();
        let func = match &handler {
            Val::AsyncNativeFn { func, .. } => func.clone(),
            _ => panic!("expected AsyncNativeFn"),
        };

        for _ in 0..2 {
            let data = Val::List(vec![Val::Str("cached".into())]);
            let result = Rc::new(RefCell::new(None));
            let result_clone = result.clone();
            let resume = Val::NativeFn {
                name: "test-resume".into(),
                func: Rc::new(move |args: &[Val]| {
                    *result_clone.borrow_mut() = Some(args[0].clone());
                    Ok(Val::Nil)
                }),
            };

            let fut = func(vec![data, resume]);
            let waker = std::task::Waker::noop();
            let mut cx = std::task::Context::from_waker(&waker);
            let mut pinned = fut;
            match std::pin::Pin::new(&mut pinned).poll(&mut cx) {
                std::task::Poll::Ready(Ok(_)) => {}
                other => panic!("unexpected poll result: {other:?}"),
            }

            let r = result.borrow();
            match r.as_ref().unwrap() {
                Val::Map(entries) => assert_eq!(entries.len(), 1),
                other => panic!("expected Map, got {other}"),
            }
        }

        clear_import_cache();
    }

    #[test]
    fn import_missing_file_returns_error() {
        clear_import_cache();

        let handler = make_import_handler();
        let func = match &handler {
            Val::AsyncNativeFn { func, .. } => func.clone(),
            _ => panic!("expected AsyncNativeFn"),
        };

        let data = Val::List(vec![Val::Str("nonexistent".into())]);
        let resume = Val::NativeFn {
            name: "test-resume".into(),
            func: Rc::new(move |_args: &[Val]| Ok(Val::Nil)),
        };

        let fut = func(vec![data, resume]);
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(&waker);
        let mut pinned = fut;
        match std::pin::Pin::new(&mut pinned).poll(&mut cx) {
            std::task::Poll::Ready(Err(_)) => {} // expected — file doesn't exist
            std::task::Poll::Ready(Ok(_)) => panic!("expected error for missing file"),
            std::task::Poll::Pending => panic!("unexpected pending"),
        }

        clear_import_cache();
    }

    // -- call_resume --

    #[test]
    fn call_resume_with_native_fn() {
        let resume = Val::NativeFn {
            name: "test-resume".into(),
            func: std::rc::Rc::new(|args: &[Val]| Ok(args[0].clone())),
        };
        let result = call_resume(&resume, Val::Int(42));
        assert_eq!(result, Ok(Val::Int(42)));
    }

    #[test]
    fn call_resume_with_non_fn_errors() {
        let result = call_resume(&Val::Nil, Val::Int(1));
        assert!(result.is_err());
        let result = call_resume(&Val::Int(5), Val::Str("x".into()));
        assert!(result.is_err());
    }

    // -- extract_method --

    #[test]
    fn extract_method_valid() {
        let data = Val::List(vec![
            Val::Keyword("peers".into()),
            Val::Int(1),
            Val::Str("extra".into()),
        ]);
        let (method, rest) = extract_method(&data).unwrap();
        assert_eq!(method, "peers");
        assert_eq!(rest, vec![Val::Int(1), Val::Str("extra".into())]);
    }

    #[test]
    fn extract_method_keyword_only() {
        let data = Val::List(vec![Val::Keyword("id".into())]);
        let (method, rest) = extract_method(&data).unwrap();
        assert_eq!(method, "id");
        assert!(rest.is_empty());
    }

    #[test]
    fn extract_method_non_list_errors() {
        assert!(extract_method(&Val::Int(5)).is_err());
        assert!(extract_method(&Val::Nil).is_err());
    }

    #[test]
    fn extract_method_non_keyword_head_errors() {
        let data = Val::List(vec![Val::Str("not-keyword".into())]);
        assert!(extract_method(&data).is_err());
    }

    #[test]
    fn extract_method_empty_list_errors() {
        assert!(extract_method(&Val::List(vec![])).is_err());
    }

    // -- wrap_with_handlers --

    #[test]
    fn wrap_with_handlers_no_extras() {
        let form = Val::Int(42);
        let wrapped = wrap_with_handlers(&form, &[]);
        // Should be nested: host(fs(routing(import(:load(42)))))
        // Outermost is host
        match &wrapped {
            Val::List(items) => {
                assert_eq!(items[0], Val::Sym("with-effect-handler".into()));
                assert_eq!(items[1], Val::Sym("host".to_string()));
            }
            other => panic!("expected list, got {other:?}"),
        }
    }

    #[test]
    fn wrap_with_handlers_with_extras() {
        let form = Val::Int(1);
        let wrapped = wrap_with_handlers(&form, &["custom"]);
        // "custom" is an extra cap — should appear in the wrapping
        match &wrapped {
            Val::List(items) => {
                assert_eq!(items[0], Val::Sym("with-effect-handler".into()));
                // The outermost is still host (extras are inserted at index 0
                // of the caps vec, so they're INNER, not outer)
                assert_eq!(items[1], Val::Sym("host".to_string()));
            }
            other => panic!("expected list, got {other:?}"),
        }
    }

    // -- make_fs_handler --
    //
    // Drive the handler against real local files in a tempdir. In a WASM
    // cell std::fs would be backed by the WASI VFS (CidTree at runtime);
    // here it's the host's real FS, but the handler logic is identical.
    // Each test sets WW_ROOT to the tempdir so relative paths resolve
    // against it without leaking into the host's actual root.

    /// Drive an `AsyncNativeFn` synchronously by polling its future to
    /// completion on a fresh single-threaded tokio runtime.
    fn drive_handler(handler: &Val, method: &str, path: Option<&str>) -> Result<Val, Val> {
        let mut method_list = vec![Val::Keyword(method.into())];
        if let Some(p) = path {
            method_list.push(Val::Str(p.into()));
        }
        let resume = Val::NativeFn {
            name: "test-resume".into(),
            func: std::rc::Rc::new(|args: &[Val]| Ok(args[0].clone())),
        };
        let args = vec![Val::List(method_list), resume];

        match handler {
            Val::AsyncNativeFn { func, .. } => {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("runtime");
                let local = tokio::task::LocalSet::new();
                local.block_on(&rt, func(args))
            }
            other => panic!("expected AsyncNativeFn, got {other:?}"),
        }
    }

    /// Cargo runs tests in parallel, but `WW_ROOT` is a process-wide env
    /// var and `make_fs_handler` reads it at call time. Without
    /// serialization, two tests setting `WW_ROOT` at the same time race
    /// and one fixture's path resolves against the other's tempdir.
    /// All `with_fixture` callers acquire this mutex.
    static FS_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Set up a tempdir, populate it with a known fixture, point WW_ROOT
    /// at it, run the closure, then restore WW_ROOT. Serialized via
    /// `FS_TEST_LOCK` so concurrent tests don't clobber each other's
    /// env-var state.
    fn with_fixture<F: FnOnce(&std::path::Path)>(f: F) {
        let _guard = FS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("hello.txt"), b"hi").unwrap();
        std::fs::write(dir.path().join("data.bin"), [0xff_u8, 0xfe, 0xfd]).unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub").join("nested.txt"), b"nested").unwrap();

        let prev = std::env::var("WW_ROOT").ok();
        std::env::set_var("WW_ROOT", dir.path());
        f(dir.path());
        match prev {
            Some(v) => std::env::set_var("WW_ROOT", v),
            None => std::env::remove_var("WW_ROOT"),
        }
    }

    #[test]
    fn fs_read_returns_bytes() {
        let handler = make_fs_handler();
        with_fixture(|_| {
            let result = drive_handler(&handler, "read", Some("hello.txt")).unwrap();
            assert_eq!(result, Val::Bytes(b"hi".to_vec()));
        });
    }

    #[test]
    fn fs_read_str_returns_utf8_string() {
        let handler = make_fs_handler();
        with_fixture(|_| {
            let result = drive_handler(&handler, "read-str", Some("hello.txt")).unwrap();
            assert_eq!(result, Val::Str("hi".into()));
        });
    }

    #[test]
    fn fs_read_str_invalid_utf8_yields_structured_error() {
        let handler = make_fs_handler();
        with_fixture(|_| {
            let err = drive_handler(&handler, "read-str", Some("data.bin")).unwrap_err();
            assert_eq!(
                glia::error::type_tag(&err),
                Some(glia::error::tag::CAP_CALL),
                "expected cap-call-failed tag, got: {err:?}"
            );
        });
    }

    #[test]
    fn fs_ls_returns_entries_with_name_size_type() {
        let handler = make_fs_handler();
        with_fixture(|_| {
            let result = drive_handler(&handler, "ls", Some("")).unwrap();
            let items = match result {
                Val::List(xs) => xs,
                other => panic!("expected list, got {other:?}"),
            };
            assert_eq!(items.len(), 3, "fixture has 3 entries (2 files, 1 dir)");

            // Each entry should be a map with :name :size :type
            for entry in &items {
                let m = match entry {
                    Val::Map(m) => m,
                    other => panic!("expected map, got {other:?}"),
                };
                assert!(m.contains_key(&Val::Keyword("name".into())));
                assert!(m.contains_key(&Val::Keyword("size".into())));
                let ty = m
                    .get(&Val::Keyword("type".into()))
                    .expect(":type field present");
                assert!(matches!(
                    ty,
                    Val::Keyword(k) if k == "file" || k == "dir" || k == "symlink"
                ));
            }
        });
    }

    #[test]
    fn fs_stat_returns_size_and_type() {
        let handler = make_fs_handler();
        with_fixture(|_| {
            let result = drive_handler(&handler, "stat", Some("hello.txt")).unwrap();
            let m = match result {
                Val::Map(m) => m,
                other => panic!("expected map, got {other:?}"),
            };
            assert_eq!(
                m.get(&Val::Keyword("size".into())),
                Some(&Val::Int(2)),
                "hello.txt is 2 bytes"
            );
            assert_eq!(
                m.get(&Val::Keyword("type".into())),
                Some(&Val::Keyword("file".into()))
            );
        });
    }

    #[test]
    fn fs_stat_dir_reports_dir_type() {
        let handler = make_fs_handler();
        with_fixture(|_| {
            let result = drive_handler(&handler, "stat", Some("sub")).unwrap();
            let m = match result {
                Val::Map(m) => m,
                other => panic!("expected map, got {other:?}"),
            };
            assert_eq!(
                m.get(&Val::Keyword("type".into())),
                Some(&Val::Keyword("dir".into()))
            );
        });
    }

    #[test]
    fn fs_exists_returns_true_for_present_path() {
        let handler = make_fs_handler();
        with_fixture(|_| {
            let result = drive_handler(&handler, "exists?", Some("hello.txt")).unwrap();
            assert_eq!(result, Val::Bool(true));
        });
    }

    #[test]
    fn fs_exists_returns_false_for_missing_path_without_error() {
        let handler = make_fs_handler();
        with_fixture(|_| {
            // Critical: missing paths must NOT error — the whole point of
            // exists? is to query without raising.
            let result = drive_handler(&handler, "exists?", Some("does-not-exist.txt")).unwrap();
            assert_eq!(result, Val::Bool(false));
        });
    }

    #[test]
    fn fs_read_missing_path_yields_structured_error() {
        let handler = make_fs_handler();
        with_fixture(|_| {
            let err = drive_handler(&handler, "read", Some("nope.txt")).unwrap_err();
            assert_eq!(
                glia::error::type_tag(&err),
                Some(glia::error::tag::CAP_CALL),
                "expected cap-call-failed tag, got: {err:?}"
            );
        });
    }

    #[test]
    fn fs_unknown_method_yields_structured_error() {
        let handler = make_fs_handler();
        with_fixture(|_| {
            let err = drive_handler(&handler, "explode", Some("anything")).unwrap_err();
            assert_eq!(
                glia::error::type_tag(&err),
                Some(glia::error::tag::CAP_CALL)
            );
            // Message should mention the unknown method name to help debugging.
            let msg = glia::error::message(&err).unwrap_or("");
            assert!(msg.contains("explode"), "msg should mention method: {msg}");
        });
    }

    #[test]
    fn fs_missing_path_arg_yields_arity_error() {
        let handler = make_fs_handler();
        let err = drive_handler(&handler, "read", None).unwrap_err();
        assert_eq!(
            glia::error::type_tag(&err),
            Some(glia::error::tag::ARITY)
        );
    }
}
