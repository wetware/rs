# Glia Shell

The wetware kernel (`pid0`) provides two runtime modes, selected
automatically based on whether standard input is a terminal.

## Interactive mode (TTY)

When `ww run` detects a TTY on stdin it sets `WW_TTY=1` in the guest
environment. The kernel starts a Clojure-inspired Lisp REPL called
**Glia**:

```
/ > (host id)
"12D3KooW..."
/ > (executor echo "hello")
"hello"
/ > (exit)
```

Glia is a full language, not just a command dispatcher. It has lexical
closures, macros, pattern matching, and an algebraic effect system that
the capability model is built on top of.


## Data types

| Syntax | Type | Example |
|--------|------|---------|
| `nil` | Nil | `nil` |
| `true` / `false` | Bool | `true` |
| integers | Int | `42`, `-7` |
| floats | Float | `3.14`, `1e10` |
| `"double-quoted"` | Str | `"hello world"` |
| bare words | Sym | `foo`, `my-fn` |
| `:colon-prefixed` | Keyword | `:port`, `:ok` |
| `(a b c)` | List | `(+ 1 2)` |
| `[a b c]` | Vector | `[1 2 3]` |
| `{k1 v1 k2 v2}` | Map | `{:name "Alice" :age 30}` |
| `#{a b c}` | Set | `#{:a :b :c}` |

Commas are whitespace (ignored). Line comments start with `;` and run
to end of line.

Truthiness follows Clojure: only `nil` and `false` are falsy.
Everything else is truthy, including `0`, empty strings, and empty
collections.


## Special forms

Special forms receive their arguments **unevaluated**. They cannot be
passed as values or stored in variables.

### def

Bind a name in the root (global) scope:

```clojure
(def x 42)
(def y)          ;; binds y to nil
```

### if

Conditional with lazy branch evaluation:

```clojure
(if (> x 0) "positive" "non-positive")
(if condition then-expr)   ;; else branch defaults to nil
```

### do

Evaluate forms sequentially, return the last:

```clojure
(do
  (println "step 1")
  (println "step 2")
  :done)
```

### let

Local scope with sequential bindings. Supports destructuring (see
Pattern matching below):

```clojure
(let [x 1
      y (+ x 1)]
  (+ x y))

;; destructuring
(let [[a b] [1 2]]
  (+ a b))
```

### fn

Create a closure. Single-arity or multi-arity:

```clojure
;; single arity
(fn [x y] (+ x y))

;; variadic
(fn [x & rest] rest)

;; multi-arity
(fn
  ([x] (+ x 1))
  ([x y] (+ x y)))
```

Closures capture their definition-time environment (lexical scope).
The `& rest` parameter collects remaining arguments as a list.

### loop / recur

Tail-recursive iteration. `recur` re-binds loop variables and jumps
back to the loop head:

```clojure
(loop [i 0 acc 0]
  (if (> i 10)
    acc
    (recur (+ i 1) (+ acc i))))
```

`recur` also works inside `fn` bodies to recurse on the enclosing
function.

### quote

Return the form as data, unevaluated:

```clojure
(quote (+ 1 2))   ;; => (+ 1 2)
'(+ 1 2)          ;; reader shorthand for quote
```

### Syntax-quote (quasiquote)

Build code templates with selective evaluation. Uses backtick for
quoting, `~` for unquote, and `~@` for splice-unquote:

```clojure
`(list ~x ~@ys)
;; If x = 1 and ys = (2 3), produces: (list 1 2 3)
```

This is the foundation of the macro system.

### defmacro

Define a macro. Like `fn`, but receives unevaluated arguments and
its return value is re-evaluated in the caller's scope:

```clojure
(defmacro unless [test & body]
  `(if ~test nil (do ~@body)))

(unless false (println "ran"))  ;; prints "ran"
```

Supports multi-arity just like `fn`.

### match

Pattern matching with destructuring. First matching clause wins:

```clojure
(match response
  {:ok value}   (println "got" value)
  {:err reason} (println "failed:" reason)
  _             (println "unknown"))
```

See the Pattern matching section for full pattern syntax.

### perform

Signal an effect. Two forms:

```clojure
;; keyword effect (environmental)
(perform :fail "something went wrong")

;; cap-targeted effect (object-scoped)
(perform host :id)
(perform executor :echo "hello")
```

Cap-targeted performs are the mechanism behind capability method calls.
See Capabilities and Effect handlers below.

### with-effect-handler

Install an effect handler for a body of expressions:

```clojure
;; keyword handler
(with-effect-handler :fail
  (fn [err] {:error err})
  (perform :fail "boom"))

;; with resume continuation
(with-effect-handler :fail
  (fn [err resume] (resume :recovered))
  (perform :fail "boom"))

;; cap handler
(with-effect-handler some-cap
  handler-fn
  (perform some-cap :method args...))
```

### apply

Call a function with a spread argument list:

```clojure
(apply + [1 2 3])        ;; => 6
(apply my-fn "a" [1 2])  ;; like (my-fn "a" 1 2)
```


## Built-in functions

### Collections

| Function | Signature | Description |
|----------|-----------|-------------|
| `list` | `(list args...)` | Create a list from arguments |
| `cons` | `(cons x coll)` | Prepend x to a list/vector, returns a list |
| `first` | `(first coll)` | First element (nil if empty) |
| `rest` | `(rest coll)` | All but the first element (empty list if empty) |
| `count` | `(count coll)` | Number of elements (works on lists, vectors, maps, sets, strings) |
| `vec` | `(vec coll)` | Convert list to vector |
| `get` | `(get coll key)` | Look up key in map or index in vector; nil if missing |
| `assoc` | `(assoc m k v ...)` | Associate key-value pairs into a map |
| `conj` | `(conj coll x ...)` | Add to collection (appends to vectors, prepends to lists, merges into maps) |
| `concat` | `(concat coll ...)` | Concatenate sequences into a list |
| `contains?` | `(contains? coll key)` | Key presence in map/set, or index-in-bounds for vector |

```clojure
(cons 0 [1 2 3])         ;; => (0 1 2 3)
(first [10 20 30])       ;; => 10
(rest [10 20 30])        ;; => (20 30)
(get {:a 1 :b 2} :a)    ;; => 1
(assoc {:a 1} :b 2)     ;; => {:a 1 :b 2}
(conj [1 2] 3)           ;; => [1 2 3]
(conj '(1 2) 0)          ;; => (0 1 2)
(conj {:a 1} [:b 2])     ;; => {:a 1 :b 2}
(concat [1 2] [3 4])     ;; => (1 2 3 4)
(contains? {:a 1} :a)    ;; => true
(contains? [10 20] 0)    ;; => true (index 0 exists)
```

### Arithmetic

| Function | Description |
|----------|-------------|
| `+` | Addition (variadic, starts at 0) |
| `-` | Subtraction (unary negation with 1 arg) |
| `*` | Multiplication (variadic, starts at 1) |
| `/` | Division (2 args, errors on zero) |
| `mod` | Modulo (2 args, errors on zero) |

Ints and floats can be mixed; the result promotes to float when
either operand is a float.

```clojure
(+ 1 2 3)     ;; => 6
(- 10 3)      ;; => 7
(- 5)         ;; => -5
(* 2 3.0)     ;; => 6.0
(/ 10 3)      ;; => 3
(mod 10 3)    ;; => 1
```

### Comparison

| Function | Description |
|----------|-------------|
| `=` | Structural equality (any two values) |
| `<` | Less than (numbers only) |
| `>` | Greater than (numbers only) |
| `<=` | Less than or equal |
| `>=` | Greater than or equal |

All comparison operators take exactly 2 arguments.

### Type checking

| Function | Description |
|----------|-------------|
| `type` | Returns a keyword for the type: `:nil`, `:bool`, `:int`, `:float`, `:str`, `:sym`, `:keyword`, `:list`, `:vector`, `:map`, `:set`, `:bytes`, `:fn`, `:cap`, etc. |
| `nil?` | True if value is nil |
| `some?` | True if value is not nil |
| `empty?` | True if collection/string is empty (nil counts as empty) |
| `contains?` | Key presence (see Collections above) |

```clojure
(type 42)          ;; => :int
(type :foo)        ;; => :keyword
(nil? nil)         ;; => true
(some? 0)          ;; => true
(empty? [])        ;; => true
(empty? "")        ;; => true
```

### Strings

| Function | Description |
|----------|-------------|
| `str` | Concatenate arguments into a string. Nil produces empty string. |
| `name` | Extract string from a keyword or symbol |
| `println` | Print arguments separated by spaces, returns nil |

```clojure
(str "hello " "world")  ;; => "hello world"
(str "count: " 42)      ;; => "count: 42"
(name :foo)              ;; => "foo"
(println "hello" "world") ;; prints: hello world
```

### Other

| Function | Description |
|----------|-------------|
| `gensym` | Generate a unique symbol (for macro hygiene) |
| `ex-info` | Create an error map: `(ex-info "message" {:key val})` adds `:message` to the map |

```clojure
(gensym)                         ;; => G__1
(ex-info "failed" {:code 404})   ;; => {:code 404 :message "failed"}
```


## Higher-order functions

| Function | Signature | Description |
|----------|-----------|-------------|
| `map` | `(map f coll)` | Apply f to each element, return list of results |
| `filter` | `(filter pred coll)` | Keep elements where pred is truthy |
| `reduce` | `(reduce f coll)` or `(reduce f init coll)` | Left fold |

```clojure
(map (fn [x] (* x x)) [1 2 3 4])
;; => (1 4 9 16)

(filter (fn [x] (> x 2)) [1 2 3 4 5])
;; => (3 4 5)

(reduce + [1 2 3 4])
;; => 10

(reduce + 100 [1 2 3])
;; => 106
```


## Prelude

The prelude is loaded at boot from `prelude.glia`. These are macros
written in Glia itself:

### defn

Define a named function (sugar for `def` + `fn`):

```clojure
(defn square [x] (* x x))
(square 5)  ;; => 25
```

### Logical operators

```clojure
(not true)          ;; => false
(and true false)    ;; => false (short-circuits)
(or nil false 42)   ;; => 42   (short-circuits)
```

### Conditional forms

```clojure
(when (> x 0)
  (println "positive")
  x)

(when-not (empty? items)
  (first items))

(cond
  (< x 0)  "negative"
  (= x 0)  "zero"
  true      "positive")
```

### Error handling

Error handling is sugar over the effect system, using the `:fail`
keyword effect:

```clojure
;; throw — signal a :fail effect
(throw "something went wrong")

;; try — catch failures as {:ok val} or {:err data}
(try (/ 10 0))
;; => {:err "division by zero"}

(try (+ 1 2))
;; => {:ok 3}

;; or-else — default value on failure
(or-else (/ 10 0) :default)
;; => :default

;; guard — assert or throw
(guard (> x 0) "x must be positive")

;; try-resume — handle errors with a recovery function
;; The recover-fn receives (err, resume).
;; Call (resume value) to continue, or return to abort.
(try-resume
  (fn [err resume] (resume 0))
  (+ 1 (throw "oops")))
```


## Capabilities

Capabilities are first-class values in the Glia environment. The
kernel binds four capabilities at boot: `host`, `executor`, `ipfs`,
and `routing`. Each is a `Cap` value with a schema CID that identifies
its interface type.

Capability methods are invoked via the effect system. In the
interactive shell, every expression is automatically wrapped in
`with-effect-handler` for each capability, so you can use the
shorthand syntax:

```clojure
;; Shorthand (shell wraps in handlers automatically):
(host id)

;; Equivalent explicit form:
(perform host :id)
```

Scripts loaded via `init.d` are also wrapped. In user-defined handler
contexts, you use the explicit `(perform cap :method args...)` form.

### host

| Method | Example | Description |
|--------|---------|-------------|
| `:id` | `(host id)` | Peer ID (base58-encoded) |
| `:addrs` | `(host addrs)` | Listen multiaddrs as a list of strings |
| `:peers` | `(host peers)` | Connected peers — list of `{:peer-id "..." :addrs (...)}` maps |
| `:listen` | `(host listen executor wasm)` | Register a VatListener (RPC handler) using the WASM binary's embedded schema |
| `:listen` | `(host listen executor "proto" wasm)` | Register a StreamListener for protocol `"proto"` |

```clojure
(host id)
;; => "12D3KooWAbC..."

(host addrs)
;; => ("/ip4/127.0.0.1/tcp/2025" "/ip4/192.168.1.5/tcp/2025")

(host peers)
;; => ({:peer-id "12D3KooW..." :addrs ("/ip4/...")})

;; Register an RPC handler (schema CID is read from the WASM custom section):
(def wasm (load "bin/my-handler.wasm"))
(host listen executor wasm)

;; Register a raw stream handler:
(host listen executor "my-protocol" wasm)
```

### executor

| Method | Example | Description |
|--------|---------|-------------|
| `:echo` | `(executor echo "hello")` | Diagnostic echo — round-trips a string through host RPC |
| `:run` | `(executor run wasm)` | Spawn a foreground WASM process, wait for exit, return exit code |

```clojure
(executor echo "hello")
;; => "hello"

(def wasm (load "bin/my-tool.wasm"))
(executor run wasm)
;; => 0

;; With environment variables:
(executor run wasm :env {"KEY" "VALUE" "OTHER" "data"})
;; => 0
```

### ipfs

IPFS methods pipeline through the UnixFS sub-API.

| Method | Example | Description |
|--------|---------|-------------|
| `:cat` | `(ipfs cat "/ipfs/Qm...")` | Fetch content as bytes. Relative paths resolve against `$WW_ROOT`. |
| `:ls` | `(ipfs ls "/ipfs/Qm...")` | List directory entries as `(("name" size) ...)` |

```clojure
(ipfs cat "etc/config.json")
;; => <bytes>

(ipfs ls "/")
;; => (("bin" 0) ("etc" 0) ("boot" 0))
```

### routing

DHT-based discovery. Names are hashed to CIDs internally, so you work
with human-readable strings.

| Method | Example | Description |
|--------|---------|-------------|
| `:provide` | `(routing provide "my-service")` | Announce this node as a provider for the given name |
| `:find` | `(routing find "my-service")` | Find providers — returns list of `{:peer-id "..." :addrs (...)}` |
| `:hash` | `(routing hash "data")` | Hash data to a CID string |

```clojure
(routing provide "greeter/v1")
;; => nil

(routing find "greeter/v1")
;; => ({:peer-id "12D3KooW..." :addrs ("/ip4/...")})

(routing find "greeter/v1" :count 5)
;; => (up to 5 providers)

(routing hash "greeter/v1")
;; => "bafk..."
```

### identity

The `identity` capability is declared in the session (provides
host-side signing via `stem_capnp::identity::Client`) but does not
yet have an effect handler wired up. It is reserved for future use
— the private key never enters WASM; only the signing capability
reference is passed.


## Effect handlers

The effect system is Glia's mechanism for structured side effects.
It replaces both exceptions and capability dispatch with a single
unified model.

### How it works

1. **`perform`** signals an effect. It suspends the current
   computation and walks the handler stack looking for a match.

2. **`with-effect-handler`** installs a handler on the dynamic
   (not lexical) handler stack. When a matching `perform` fires,
   the handler function is called.

3. **`resume`** is a one-shot continuation. If the handler function
   accepts 2 arguments `(data, resume)`, calling `(resume value)`
   sends `value` back to the `perform` call site and the body
   continues executing.

### Keyword effects

Keyword effects are environmental/global. Any code can perform them
and any enclosing handler can catch them:

```clojure
(with-effect-handler :log
  (fn [msg] (println "LOG:" msg))
  (do
    (perform :log "step 1")
    (perform :log "step 2")))
```

### Cap-targeted effects

Cap effects match by schema CID. Two capabilities with the same
Cap'n Proto interface type share a handler, regardless of their
instance identity:

```clojure
(with-effect-handler my-cap
  handler-fn
  (perform my-cap :method arg1 arg2))
```

### Resumable effects

When the handler takes two arguments, the second is a one-shot
`resume` function:

```clojure
(with-effect-handler :ask
  (fn [question resume]
    (resume "42"))
  (let [answer (perform :ask "meaning of life?")]
    (println "answer:" answer)))
;; prints: answer: 42
```

The `resume` function can only be called once (it is a one-shot
continuation). Calling it twice produces an error.

### Handler stack

Handlers are stacked dynamically. Inner handlers shadow outer ones
for the same effect type. When a handler fires, it is popped from
the stack so that its own `perform` calls reach outer handlers (no
infinite loops).

The maximum handler stack depth is **64**. Exceeding this limit
produces an error.


## Pattern matching

The `match` form provides structural pattern matching. It is also
used internally by `let` and `fn` for destructuring.

### Patterns

| Pattern | Matches |
|---------|---------|
| `_` | Anything (wildcard, no binding) |
| `x` (symbol) | Anything, binds the value to `x` |
| `nil`, `true`, `42`, `"str"`, `:kw` | Literal equality |
| `[a b c]` | Vector/list with exactly 3 elements |
| `[a b & rest]` | Vector/list with 2+ elements, rest collects remainder |
| `{:key pat}` | Map with `:key` present, match pat against its value |

Map patterns do **partial/open matching** — extra keys in the value
are ignored. Vector patterns require an exact element count unless
`& rest` is used.

### Examples

```clojure
;; Literal and wildcard
(match x
  0     "zero"
  1     "one"
  _     "other")

;; Vector destructuring
(match [1 2 3]
  [a b c] (+ a b c))   ;; => 6

;; Rest patterns
(match [1 2 3 4 5]
  [head & tail] {:head head :tail tail})
;; => {:head 1 :tail (2 3 4 5)}

;; Map destructuring
(match {:name "Alice" :age 30}
  {:name n :age a} (str n " is " a))
;; => "Alice is 30"

;; Nested patterns
(match {:coords [1.0 2.0]}
  {:coords [lat lng]} (str lat "," lng))
;; => "1.0,2.0"

;; let destructuring (same pattern language)
(let [{:name name} {:name "Bob" :extra true}]
  name)
;; => "Bob"

;; fn parameter destructuring uses the same system
(defn point-x [[x _]] x)
(point-x [3 4])  ;; => 3
```


## Built-in commands

These are dispatched directly by the kernel, not through the effect
system:

| Command | Description |
|---------|-------------|
| `(cd "<path>")` | Change the shell's working directory |
| `(help)` | Print available capabilities, effects, and built-ins |
| `(exit)` | Terminate the kernel process |
| `(load "<path>")` | Read bytes from the WASI virtual filesystem (cached) |

`load` is also available as a keyword effect: `(perform :load "path")`
works inside `with-effect-handler` contexts.


## PATH lookup

Any command that is not a known capability, built-in, or user-defined
function triggers a PATH lookup. The kernel scans each directory in
`PATH` (default: `/bin`) for two candidates in order:

1. `<dir>/<cmd>.wasm` — flat single-file binary
2. `<dir>/<cmd>/main.wasm` — image-style nested binary

The first match wins. The WASM is passed to `executor.runBytes()`.
Standard output is captured and printed; the exit code is reported on
error.

```
/ > (my-tool "arg1" "arg2")
```

This looks for `/bin/my-tool.wasm` or `/bin/my-tool/main.wasm` in the
image filesystem. The nested form lets a command be a full FHS image
directory with its own `boot/`, `etc/`, etc.


## init.d scripts

Before the interactive shell starts, the kernel scans
`$WW_ROOT/etc/init.d/` for `*.glia` files and evaluates them in
alphabetical order. Each script runs with the same capability
handlers as the shell.

If any init.d expression blocks (e.g. `(executor run ...)` runs a
foreground process to completion), the kernel exits after init.d
finishes and does not enter interactive mode.


## Daemon mode (non-TTY)

When stdin is not a terminal, the kernel enters daemon mode:

1. Grafts onto the host membrane and obtains a session
2. Logs a JSON readiness message to stderr:
   ```json
   {"event":"ready","peer_id":"0024080112..."}
   ```
3. Blocks on stdin until the host closes it (signaling shutdown)
4. All stdin data is discarded; no interactive input is processed

Daemon mode is used for headless nodes. The init.d scripts still
run before the daemon blocks, so a typical pattern is to register
listeners and providers in init.d and then let the daemon keep the
process alive.
