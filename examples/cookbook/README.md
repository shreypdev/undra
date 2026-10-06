# The Undra cookbook

One recipe per module of one Rust crate, each tested against the test runtime and the deterministic fakes. The docs
site's [cookbook pages](https://shreypdev.github.io/undra/docs/cookbook/) quote this code, so a recipe is checked by the
compiler and by `cargo test`, not only read.

| Module (`core/src/`) | Recipe |
|---|---|
| `net.rs` | where the server is, and what a failed request is (shared) |
| `auth.rs` | a session store, tokens in `SecureStore`, a 401 that re-authenticates, a logout that clears state |
| `paging.rs` | a keyed list fed page by page, a derived view, the cursor in a signal |
| `forms.rs` | a signal per field, errors derived from them, a command that refuses typed |
| `upload.rs` | a file from `Fs` in parts over `Http`, progress as a signal, retries through the offline queue |
| `offline.rs` | persisted queries, writes that queue, an outbox, an update (`#[undra(default)]`, `#[undra::migrate]`) |
| `leaderboard.rs`, `standings.rs` | a ranking of 100,000 players from a snapshot and a delta stream: ingested on `spawn_blocking` into a structure the app owns, published as a window of 60 rows and four numbers in one transaction (`standings.rs` is plain Rust; the benchmark harness compiles it too) |
| `realtime.rs` | a WebSocket that reconnects in the core, server-sent events as the fallback (feature `realtime`) |

One recipe has no Rust module, because it is about what is under the ports, not what is above them: **Your network stack**
(`site/docs/cookbook/network-stack.html`, ADR-060), the app's `OkHttpClient`, `URLSession` or `fetch` behind the `Http`, `WebSocket`
and `Sse` ports. Its Swift and TypeScript lines are in `snippets/` like the others; its Kotlin lines need OkHttp, which this check
does not have, so they are compiled and run with the `okhttp-adapters` module
(`runtimes/kotlin/undra-runtime/okhttp-adapters/src/test/kotlin/dev/undra/okhttp/NetworkStackRecipe.kt`, and its test).

```sh
cargo test -p cookbook                                   # every recipe, against the fakes
undra bindgen -C examples/cookbook --check --docs        # the bindings in generated/ are what the core generates
bash examples/cookbook/snippets/check.sh                 # the Swift, Kotlin and TypeScript lines the pages show compile
```

`snippets/` holds those lines (`docs:begin` / `docs:end` mark what a page quotes; `site/scripts/build-cookbook.mjs`
copies it into the pages). `cargo test -p cookbook --features realtime` also runs the real-time recipe; it needs an
`undra` that has the opt-in `WebSocket` and `Sse` ports (ADR-047), and is on in CI once they are on `main`. The recipes
come together in [Fieldbook](../fieldbook), the sample app.
