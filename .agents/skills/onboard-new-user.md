# Onboard New User

One-time setup and orientation for someone who just arrived.

## First: ask what brought them here

Before setup steps, find out what the user actually wants.  Ask
something like:

> Welcome to Wetware!  Before we set anything up — what brought you
> here?  Are you exploring, building something specific, or just
> curious how it works?

Their answer tells you how deep to go and where to hand off after
setup.  Remember it — you'll use it at the end.

## Detect environment

Figure out where the user is:

- **Local clone**: they can run commands, build things, and you can
  read files directly.
- **Browsing on GitHub**: no terminal, no local files.  Adapt —
  link to raw files, skip build steps until they clone.

---

## For GitHub browsers

If the user hasn't cloned yet:

1. Give a two-sentence intro to Wetware (peer-to-peer OS for agents,
   capability-secured, no ambient authority).
2. Explain the Cell type system briefly — it's the first thing that
   makes Wetware different.
3. Invite them to clone when they're ready:
   ```
   git clone https://github.com/wetware/ww.git && cd ww
   ```
4. Tell them once they've cloned, they can ask their AI coding tool:
   > Read .agents/prompt.md and get me started with Wetware.
5. Meanwhile, offer to walk them through concepts or examples using
   the embedded context in `prompt.md`.  Read
   `.agents/skills/explain-concepts.md` if they want concepts.

Don't push them to clone — some people want to read first.  That's
fine.

---

## For local clones

Three steps.  Tell them up front: "Three quick steps, then you'll
have a running node.  ~5 minutes total."

### Step 1 of 3: Prerequisites (~30 seconds)

Check (or tell the user to check):
- Rust toolchain installed
- `wasm32-wasip2` target: `rustup target add wasm32-wasip2`
- Optional: [Kubo](https://docs.ipfs.tech/install/) for IPFS

If something's missing, help them fix it before moving on.
Don't just list requirements and hope for the best.

### Step 2 of 3: Build (~2 minutes)

```
make
```

Builds host binary, kernel, shell, and examples.  First build is
slower — tell them it's normal.

**While it builds**, you can preview what comes next: "Once this
finishes, we'll boot a node and you'll have a live p2p shell."

### Step 3 of 3: Quick tour (~1 minute)

```
cargo run -- run crates/kernel
```

This drops into the Glia shell.  Walk them through a few commands,
one at a time:

1. `(host id)` — "That's your peer identity.  Unique to this node."
2. `(host peers)` — "Connected peers.  Probably empty since you're
   running solo."
3. `(executor echo "hello")` — "Round-trip RPC through the kernel.
   If this works, the whole stack is live."
4. `(exit)` — done.

**Name the win**: "You just booted a p2p capability-secured OS.
That's the whole runtime — host, kernel, Membrane, shell."

### Optional: what just happened?

Don't dump this unsolicited.  Ask: "Want a quick look under the
hood, or ready to move on?"

If they want it, briefly cover:
1. **Host** booted a libp2p swarm and served a Membrane
2. **Kernel** (pid0) grafted onto the Membrane to get capabilities
3. **Shell** gave you a REPL where capabilities are first-class

Point them at `doc/architecture.md` for the full picture.

---

## Hand off

Recall what brought them here (from your first question) and route
them accordingly:

- Building something specific → suggest **Build an app** (choice 5)
- Exploring / curious → suggest **Concepts** (choice 1) or
  **Examples** (choice 3)
- Not sure → present the full menu

Then show the menu from `.agents/prompt.md` and follow that file's
instructions from there.
