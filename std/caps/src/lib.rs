//! Shared Glia effect handler factories for Cap'n Proto capabilities.
//!
//! Extracted from the shell cell so that multiple cells (shell, MCP, etc.)
//! can reuse the same handler implementations without duplication.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use glia::Val;

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

pub fn extract_method(data: &Val) -> Result<(String, Vec<Val>), Val> {
    let items = match data {
        Val::List(items) => items,
        _ => return Err(Val::from("cap handler: expected list data")),
    };
    let method = match items.first() {
        Some(Val::Keyword(s)) => s.clone(),
        _ => {
            return Err(Val::from(
                "cap handler: first arg must be a keyword method (e.g. :id, :run)",
            ))
        }
    };
    Ok((method, items[1..].to_vec()))
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

                match method.as_str() {
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
                                Some(Val::Map(vec![
                                    (Val::Keyword("peer-id".into()), Val::Str(encoded)),
                                    (Val::Keyword("addrs".into()), Val::List(addrs)),
                                ]))
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

                match method.as_str() {
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

pub fn make_ipfs_handler() -> Val {
    Val::AsyncNativeFn {
        name: "ipfs-handler".into(),
        func: Rc::new(move |args: Vec<Val>| {
            Box::pin(async move {
                let (method, rest) = extract_method(&args[0])?;
                let resume = &args[1];

                match method.as_str() {
                    "cat" => {
                        let path = match rest.first() {
                            Some(Val::Str(s)) => s.clone(),
                            _ => return Err(Val::from("ipfs :cat — expected path string")),
                        };
                        let resolved = if path.starts_with("/ipfs/") {
                            path
                        } else {
                            let root = std::env::var("WW_ROOT").unwrap_or_default();
                            format!("{root}/{path}")
                        };
                        match std::fs::read(&resolved) {
                            Ok(bytes) => call_resume(resume, Val::Bytes(bytes)),
                            Err(e) => Err(Val::from(format!("ipfs :cat {resolved}: {e}"))),
                        }
                    }
                    "ls" => {
                        let path = match rest.first() {
                            Some(Val::Str(s)) => s.clone(),
                            _ => return Err(Val::from("ipfs :ls — expected path string")),
                        };
                        let resolved = if path.starts_with("/ipfs/") {
                            path
                        } else {
                            let root = std::env::var("WW_ROOT").unwrap_or_default();
                            format!("{root}/{path}")
                        };
                        match std::fs::read_dir(&resolved) {
                            Ok(entries) => {
                                let items: Vec<Val> = entries
                                    .filter_map(|e| {
                                        let e = e.ok()?;
                                        let name = e.file_name().to_string_lossy().to_string();
                                        let size = e.metadata().ok()?.len();
                                        Some(Val::Map(vec![
                                            (Val::Keyword("name".into()), Val::Str(name)),
                                            (Val::Keyword("size".into()), Val::Int(size as i64)),
                                        ]))
                                    })
                                    .collect();
                                call_resume(resume, Val::List(items))
                            }
                            Err(e) => Err(Val::from(format!("ipfs :ls {resolved}: {e}"))),
                        }
                    }
                    other => Err(Val::from(format!("ipfs: unknown method :{other}"))),
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
    let mut caps: Vec<&str> = vec!["routing", "ipfs", "host"];
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
