# UndraRuntime (Swift)

The Swift platform runtime for [Undra](../../../docs/SPEC.md): everything generated Swift code (`undra-bindgen`) depends on (docs/SPEC.md section 17.3). It contains

* the **wire layer** (section 3): byte-exact codecs, the transport envelope, every envelope payload (namespaced under `Wire`), keyed list patches and the FNV-1a identifiers;
* the **runtime core** (sections 5.1, 6, 11): `UndraCore` (in-process over a core's C ABI table, or remote over a WebSocket), `UndraCoreEntry` (what each core's generated `Undra<Namespace>` delegates to), `Mirror`, `UndraObject`, `UndraStore`, ports and typed errors;
* the **default adapters** (section 8): Http, Kv, SecureStore, Fs, Clock, Rng, Log, Timer, Connectivity, Lifecycle (from the application notifications) and Diagnostics (the core's panic reports), and, on iOS, `UndraBackground` (BackgroundTasks).
* the **opt-in ports** of ADR-047 and ADR-048: their twelve records, the adapter interfaces an app can implement (`WebSocketAdapter`, `SseAdapter`, `DbAdapter`), the bindings that serve the ports from them (`WebSocketPortAdapter`, `SsePortAdapter`, `DbPortAdapter`) and the default adapters (`URLSessionWebSocketAdapter`, `URLSessionSseAdapter`, `SQLiteDbAdapter`).

Swift 6 language mode, strict concurrency, iOS 15 / macOS 12 (generated stores are `@Observable`, iOS 17 / macOS 14, unless the app's deployment target is lower: see "iOS 15 and 16" below), no Objective-C, no third-party dependencies. Foundation, Security, Network and SQLite3 are imported only by the adapter files (`Adapters/`) and by `Wire/Foundation+Undra.swift` (the `Date` and `UUID` bridges and the `LocalizedError` conformances of the standard errors); the core itself needs only the standard library, Dispatch, `os` and libc.

## Layout

```
Package.swift
Sources/
  UndraFFI/                 C target, header only: include/undra.h (the C ABI's types, section 6),
                           include/module.modulemap, undra.c (the one source file SwiftPM needs)
  UndraRuntime/
    Wire/                  WireError, UndraWriter, UndraReader, UndraCodec, UndraTypes, Envelope,
                           Payloads (Wire.Call, Wire.Reply, ...), LazyPayloads (UndraLazyValue,
                           UndraLazyInvalidated, UndraLazyPageHeader), KeyedPatch, FNV, Foundation+Undra
    Core/
      UndraCore.swift       load/shared, callSync, call, stream, construct, observe, release, event,
                           registerPort, snapshot/restore, stats, shutdown; the transport callbacks
      Transport.swift      the internal seam: UndraTransport (host to core) and UndraInbound (core to host)
      CoreTable.swift      one core's UndraApi table, read and checked once (ABI version, size, entries)
      InprocTransport.swift    calls through that table: init with @convention(c) callbacks, malloc'd
                           port replies, one started transport per core namespace
      UndraCoreEntry.swift  the entry of one core: fills in its table and hash, holds its core
      CallSlot.swift       one in-flight call: continuation + cancellation state machine
      StreamChannel.swift  stream buffer and the 16 / 8 credit window
      Mirror.swift         change-set queue: parsed, merged per signal, applied on the main actor once
                           per frame, bounded (ADR-031)
      MirrorStats.swift    MirrorStats, DrainStats, DrainListenerRegistration
      FrameScheduler.swift when a frame drain runs: a CADisplayLink (iOS, tvOS, visionOS) or the main
                           actor's next turn (macOS); the seam tests drive by hand
      UndraObject.swift     UndraObject (handle owner) and UndraStore (mirror-registered, @MainActor)
      PortImpl.swift       PortImpl.sync / .async method tables
      StandardPorts.swift  port and method ids of the section 8 ports, the RuntimeConfig record
      StandardRecords.swift the section 8 records, public: HttpMethod, Header, HttpRequest, HttpResponse,
                           HttpError, FsError, StorageError, NetKind, UndraAppState (hand-written codecs)
      RealtimeRecords.swift WsOpened, WsMessage, WsError, SseEvent, SseError (ADR-047)
      DbRecords.swift      DbMigration, DbOpened, DbValue, DbExecuted, DbRows, DbConstraint, DbError (ADR-048)
      Adapters.swift       UndraAdapter, PortImplAdapter, Adapters (the set a core is loaded with)
      LoadOptions.swift    .inproc(api:) / .remote options
      Errors.swift         UndraReplyError, UndraPortError, UndraModeError, UndraSchemaMismatchError, ...
      CallError.swift      UndraCallError (what a generated method throws), UndraUnhandledError, the mapping
      UndraStats.swift      stats() and its small JSON walker
      Protocols.swift      UndraRecord, UndraEnum, UndraError, UndraPort
      Observe.swift        Observe.allSignals
    Lazy/                  UndraLazyList (@Observable) and UndraLazyListObject (ObservableObject) over one
                           paging engine, UndraLazyListError (ADR-043)
    Ports/                 the opt-in ports' adapter interfaces and bindings: WebSocketPort (WebSocketAdapter,
                           WebSocketConnection, WebSocketPortAdapter), SsePort (SseAdapter, SseStream,
                           SsePortAdapter, SseParser), DbPort (DbAdapter, DbConnection, DbPortAdapter),
                           PulledInbox (the pump and the core's pull of one inbound stream)
    Adapters/              PlatformDefaults (Adapters.platformDefault), HttpAdapter, KeyValueAdapters
                           (Kv, SecureStore), FsAdapter, SystemAdapters (Clock, Rng, Log), TimerAdapter,
                           ConnectivityAdapter (+ UndraLifecycle), LifecycleAdapter, DiagnosticsAdapter,
                           BackgroundEngine + UndraBackground (iOS), WebSocketTransport,
                           URLSessionWebSocketAdapter, URLSessionSseAdapter, SQLiteDbAdapter
    Support/               Guarded (mutex), OneShot (blocking hand-off), UndraLog (os.Logger)
Tests/UndraRuntimeTests/    XCTest suites and Resources/wire-vectors.json
```

## Using it

```swift
import UndraRuntime
import PlaygroundCore        // generated by undra-bindgen

// At startup: load the core through its generated entry. It passes the core's table
// (`playground_core_undra_api()`) and the schema hash of the bindings, checks both, and
// registers the default adapters.
_ = try UndraPlaygroundCore.load()

// Generated stores and objects use UndraPlaygroundCore.core as their default context.
@MainActor func makeStore() throws -> TodoStore { try TodoStore() }

// The app phase (Active, Inactive, Background) reaches the core by itself, from UIApplication /
// NSApplication notifications (LifecycleAdapter, part of `Adapters.platformDefault`). An app that reports
// its own keeps working (a state the core was just told is not repeated):
UndraLifecycle().changed(.active)

// Replace a default adapter with your own implementation of a generated port protocol:
let adapters = Adapters.platformDefault
    .replacing(portId: UndraIds.Ports.Http.portId, with: httpPortImpl(MyHttp()))
_ = try UndraPlaygroundCore.load(.inproc(adapters: adapters))
```

`UndraPlaygroundCore.load(.remote(url:))` attaches to an `undra dev` core over `ws://` instead. It performs a blocking handshake (call it once, at startup), the same `UndraCore` API works, synchronous calls (`callSync`, `construct`) block the calling thread until the dev core answers, and `snapshot`/`restore` throw `UndraModeError`. On iOS a cleartext `ws://` URL needs `NSAllowsLocalNetworking` (or an ATS exception) in the app's `Info.plist`.

A remote core **reconnects by itself** when its connection drops (ADR-051, `docs/DEV_LOOP.md`): attempt `n` waits `min(5 s, 250 ms * 2^(n-1))` less up to half of it at random (`LoadOptions.reconnect`, an `UndraReconnectPolicy`; `nil` turns it off), what was in flight fails at once with `UndraCallError.unavailable(.connectionLost)`, and every store the app observes is observed again so the mirrors converge. `core.connectionState`, the `@Observable` `core.connection.state` (for SwiftUI on iOS 17 / macOS 14 and later; `core.connectionObject`, an `ObservableObject` with a `@Published state`, on every floor), `core.connectionStates()` (an `AsyncStream`) and `LoadOptions.onConnectionChange` say what it is doing: `.connecting`, `.connected`, `.reconnecting(attempt:)`, `.closed(reason)`. A schema change (`.schemaMismatch`) or a session the dev server lost because it restarted (`.sessionLost`: load a new core and create the stores again) closes the core for good.

### Your own `URLSession` (ADR-060)

The defaults above use sessions of their own. An app that has a network stack of its own (a delegate that pins certificates or answers
authentication challenges, a configuration with its headers, proxy and `URLProtocol` classes) passes its `URLSession` to the three
network adapters, and every request the core makes goes through it:

```swift
let adapters = Adapters.platformDefault
    .replacing(HttpAdapter(session: session))
    .replacing(URLSessionWebSocketAdapter(session: session))   // connections are tasks of `session`
    .replacing(URLSessionSseAdapter(session: streamingSession))
```

* `URLSessionWebSocketAdapter(session:)` makes each connection a `session.webSocketTask`, so the session's configuration, delegate
  (server trust and the pinning built on it, challenges) and delegate queue apply to the upgrade request. The adapter is the task's
  delegate for the handshake and the close frame (`URLSessionTask.delegate`, iOS 15); the session's delegate keeps answering everything
  else. The adapter never invalidates a session it was given.
* **Not a background session.** A session made from `URLSessionConfiguration.background(withIdentifier:)` runs no WebSocket task and
  takes no task delegate, and URLSession answers either with an Objective-C exception that aborts the app. The three adapters check
  first and answer with a typed error instead: `connect` is `WsError.refused(status: nil, …)` (from `URLSessionWebSocketAdapter(session:)`
  or `(configuration:)`), `open` is `SseError.refused(status: nil, …)`, a request is `HttpError.network`. `BackgroundSessionTests` runs
  each in a child process, so the old abort is a failed test, not a dead suite.
* **Give event streams a session of their own with the same delegate**: an event stream may stay quiet for minutes and a stream holds
  its HTTP/1.1 connection, so the session needs a `timeoutIntervalForRequest` that allows a quiet stream and an
  `httpMaximumConnectionsPerHost` above the number of streams open at once (the defaults of `URLSessionSseAdapter.makeDefaultSession()`:
  a day and 1,024). Copy your configuration, set those two, and pass the same delegate (the cookbook recipe has the lines).
* `AppSessionTests` and `URLSessionWebSocketOnAppSessionTests` run Http, Sse and WebSocket on a session with a configuration header and a
  recording delegate: the header reaches the server on all three, the delegate is told of every task, the session's delegate answers
  the upgrade's authentication challenge (the adapter takes only the handshake and the close frame), and the WebSocket suite (echo,
  subprotocols, refusals with their status, the peer's close, a drop, a stalled reader) passes on it unchanged.

### iOS 15 and 16

The package's floor is iOS 15 / macOS 12 (ADR-045, `docs/IOS_15_16.md`). Three things in the runtime are newer than that and are written around: `Swift.Duration` and the clock types are iOS 16 / macOS 13 (a drain is timed with `DispatchTime`, `DrainStats.durationNanoseconds` is the field and `duration` an iOS 16 accessor, `UndraDuration` holds exact nanoseconds), and Observation is iOS 17 / macOS 14, so every runtime type that SwiftUI observes comes in two forms: `UndraConnection` (`@Observable`, `@available(iOS 17, macOS 14, *)`, `core.connection`) and its twin `UndraConnectionObject` (`ObservableObject`, `core.connectionObject`). Generated stores follow the app: `undra bindgen` writes `@Observable` classes for a deployment target of 17.0 or later and `ObservableObject`s with `@Published` properties below it (`UndraStore`, the mirror and the apply code underneath are the same). `scripts/ios-floor.sh runtime` builds this package for the iOS 15.0 simulator.

### Lazy lists (`Lazy<T>`)

A `Lazy<T>` store signal is a list the core owns and the host pages through (ADR-043): the generated store holds an `UndraLazyList<Item>` (iOS 17 / macOS 14, `@Observable`) or, in the `ObservableObject` mode, an `UndraLazyListObject<Item>` (every floor) and hands it the signal's change-set entries (`try books.applyFull(&reader)`, `try books.applyInvalidated(&reader)`). Both are `RandomAccessCollection`s of `Item?` and share one engine, so they cannot drift.

```swift
ScrollView {
    LazyVStack {
        ForEach(0..<library.books.count, id: \.self) { index in
            BookRow(books: library.books, index: index)   // reads books[index] in its own body: a placeholder until its page arrives
        }
    }
}
```

Use a lazy container, not a `List`, for a big list: SwiftUI's `List` builds every row of a `ForEach` up front (measured on iOS 26 with 10,000 rows), so it would read every page and the 24-page cache could not hold them. A `LazyVStack` builds only the rows near the screen, and so reads only their pages.

* **Reading never blocks and never crosses the boundary.** `list[index]` returns the cached row or `nil`, and *queues* a request for its page and one page on each side; the requests of a main-actor turn are sent together after it (one `callSync` page call per page in process, one `call` per page over a remote core), pages already cached or in flight are not asked for again, and indexes outside `0..<count` return `nil` and ask for nothing. `prefetch(_:)` asks for the pages of a range; `pageSize` (50) and `maxCachedPages` (24) are settable.
* **Observation.** `count` and `list[index]` register dependencies: `count` changes with the length, and a read of a row also depends on pages arriving or being replaced, so a view that reads them updates. The twin sends `objectWillChange` once per change.
* **The core changes the list.** The new length and version are taken at once; the rows already cached stay visible, stale, while the pages read since the previous change (the window, at most `maxCachedPages`, the least recently read pages go first) are asked for again: O(window), never O(list). A reply older than the list's version is dropped and asked for again, a newer one raises the length and version, a new page-server handle (a restored snapshot) drops the cache.
* **Hostile replies** (more items than the limit, truncated items, a total the page does not explain, trailing bytes) are `UndraLazyListError`s reported through `LoadOptions.onError`; the row stays `nil` and the next read asks again. Nothing traps.
* `UndraCore.stats().hostPageCalls` counts the page calls (target 3) the host has sent, whether answered, failed or cancelled: what a test or a diagnostic reads to see how much paging a screen causes.
* The list holds only the page server's handle number and the core; it releases nothing, the core's store owns the page server. `LazyListTests`, `LazyListHostileTests` and `LazyListObservationTests` run it against a fake page server.

### Threading, in one paragraph

The core calls the reply, change-set, stream and port callbacks on any of its threads, possibly holding its lock, so the runtime's trampolines copy the bytes and never call an entry of the core's table from inside them (docs/SPEC.md section 5.1). Replies resume continuations, stream items go into a `StreamChannel`, change-sets go into the mirror's queue (their entry tables parsed on the delivering thread) and are applied on the main actor at the next display frame, merged (below). `UndraCore.observe` (main actor) drains the queue before it returns, so in process a store has its real values as soon as its initializer finishes. Synchronous ports run inline on the core's thread and must not block or call into the core; asynchronous ports run in a task and answer with `undra_port_reply`.

### Behaviours worth knowing

* **Cancellation.** Cancelling the task awaiting `call` throws `CancellationError` at once and sends `undra_cancel`; a reply that races with the cancellation is dropped. Generated methods throw `CancellationError` when their task is cancelled, in every shape (ADR-032).
* **Errors** (`docs/ERRORS.md`). A generated method fails with exactly one of three things: its own typed error `E` (a Rust `Result<_, E>`), `CancellationError` (the calling task was cancelled), or an `UndraCallError` (`.cancelledByCore`, `.panicked`, `.refused`, `.unavailable`, `.malformed`) for every failure of the call itself. Nothing generated traps. A synchronous method that returns nothing and has no error type is a command (`todos.toggle(id:)`): it cannot throw, so on failure it logs at error level, calls `LoadOptions.onError` with an `UndraUnhandledError` and returns. `onError` runs on the calling thread and must not call into Undra; `onError: { assertionFailure("\($0)") }` turns reports into debug traps. The raw entry points (`callSync`, `call`, `stream`, `construct`) keep throwing `UndraReplyError` and the transport errors; `UndraCallError.mapped(_:)` is the one function that turns them into the above. `UndraCore.shared` without a loaded core (before `load` succeeds, or after `shutdown()`) is a shut-down placeholder instead of a trap: constructors and calls on it fail with `UndraCallError.unavailable(.closed)`, commands only log, and `UndraCore.current` stays `nil`.
* **Panic reports and background runs (ADR-046).** Every contained panic reaches `LoadOptions.onPanic` as one `UndraPanicReport` (message, `file:line:column`, what was running, the Rust frames as image offsets, the core's namespace, version, schema hash and image id): once per panic, in order, on the main thread, after the core has answered the call that panicked (the default `Diagnostics` adapter hops to the main queue, because the core calls the port on the thread that panicked, possibly under its lock). Without `onPanic` the runtime logs one error line. `UndraStats` gains `panicReports` and `background` (`tasks`, `pending`, `runs`, `finished`, `replayed`, `refetched`). `core.runInBackground(deadline:)` (seconds) calls the standard function `run_background` and returns an `UndraBackgroundReport`; cancelling the task cancels the call (`CancellationError`, the work done is kept). On iOS, `UndraBackground.register(taskIdentifier:loader:)` at launch registers a processing window and a refresh window with BackgroundTasks, runs the core in them, and, when the app goes to the background with work pending, asks the OS for them; its decisions live in `BackgroundEngine`, which the tests drive on macOS through fakes of `BGTask`, `BGTaskScheduler` and `UIApplication`. The testing kit has the `Diagnostics` fake, `CaptureDiagnostics` (`fakes.diagnostics.reports`).
* **Streams** are pull-based (`AsyncThrowingStream(unfolding:)`): 16 items of credit when the stream opens, topped back up to 16 whenever fewer than 8 unread items remain, so the core never runs more than about 16 items ahead of the consumer. Ending the loop early, cancelling the consumer or dropping the stream sends `undra_cancel`. Cancellation ends the iteration quietly, like `AsyncStream`. `UndraCore.stream(_:method:args:decode:mapError:)` is the same stream with the items decoded in `next()`, which is what generated stream methods return: a copy into another `AsyncThrowingStream` with `yield` would buffer without bound and the credit would no longer follow the consumer. A stream ends in the vocabulary of a failed reply (docs/SPEC.md section 3.7, ADR-036): an end item finishes the loop; an error item (flag 2) carries only the stream's own `E` and is thrown as `UndraReplyError(status: .error)`; a failed item (flag 3, `Wire.StreamFailure`: status, message, detail) is thrown as the reply it stands for, `UndraReplyError` with status `.cancelled` (a restore or a shutdown ended the stream), `.panic` (message and backtrace) or `.badRequest` (the reason). `UndraCallError.mapped(streamFailure:)` turns those into `.cancelledByCore`, `.panicked` and `.refused` exactly as for a call, and an error item on a stream without an error type into `.malformed`; `mapped(streamFailure:domain:)` decodes the error item as `E`. Nothing is read from the text of a message. A failed item whose body does not decode ends the stream with `UndraProtocolError.malformedMessage` (`.malformed`); it needs no `undra_cancel`, since the flag alone ends the stream in the core.
* **`PortImpl` closures take `[UInt8]`** (the encoded arguments), exactly what generated code writes (`{ args in var r = UndraReader(args) ... }`). A method answers a failure by throwing `UndraPortError(body:)` with the encoded error of its signature (port status 1); anything else it throws is answered "unavailable" (status 2) and logged at ERROR level naming the port, the method and the adapter, because it is a bug in the adapter (ADR-049).
* **The default stores are per core namespace (ADR-044 amendment A).** `KvAdapter()`, `FsAdapter()` and `SQLiteDbAdapter()` keep their data in `<Application Support>/<bundle id>/Undra/<namespace>/{kv,fs,db}` and `SecureStoreAdapter()` uses the Keychain service `<namespace>.dev.undra.securestore`, the namespace being that of the core they are registered with (`UndraCore.namespace`: the generated entry fills `LoadOptions.namespace` in, an in-process core without one takes its table's). Two cores of one app that both use `.platformDefault` never read or overwrite each other's keys, files, secrets or databases, and one adapter value can serve several cores. An adapter given a directory (`KvAdapter(directory:)`, `FsAdapter(root:)`, `SQLiteDbAdapter(directory:)`) or a service (`SecureStoreAdapter(service:)`) keeps it for every core: sharing a store is the app's explicit choice. The React Native module's native defaults use the same directories.
* **Storage failures are typed (ADR-049).** Every `Kv` and `SecureStore` method has the `StorageError` channel (`unavailable(String)`, `full`, `locked`, `corrupt(String)`, `io(String)`), and the default adapters answer with it, never with "unavailable": `KvAdapter` maps a full disk or quota (`NSFileWriteOutOfSpaceError`, `ENOSPC`, `EDQUOT`) to `.full`, a file data protection keeps unreadable before the first unlock (`NSFile{Read,Write}NoPermissionError` over `EPERM`) to `.locked`, an entry file that does not decode to `.corrupt`, anything else (a permission bit, `EACCES`, included) to `.io` with Foundation's message; `SecureStoreAdapter` maps `errSecInteractionNotAllowed` and `errSecAuthFailed` to `.locked`, an item that is not data and `errSecDecode` to `.corrupt`, `errSecNotAvailable` and `errSecMissingEntitlement` (no Keychain in this process) to `.unavailable`, `errSecDiskFull` to `.full`, and any other `OSStatus` to `.io` with the Security framework's message. `FsAdapter` answers a full disk with `FsError.full`. `StorageFailureTests` is the failure-injection suite the Kotlin and TypeScript runtimes mirror.
* **Snapshots are layout 2 (ADR-037).** `UndraCore.snapshot()` returns, and `restore(_:)` takes, the core's bytes unchanged; `Wire.Snapshot` decodes them for tests and tools: `generationFloor`, `schemaHash`, `types` (`Wire.SnapshotType`: each store type with the fingerprint of its signals), `description` (the core's JSON description of those types, opaque to hosts) and `stores`. A snapshot in the layout before ADR-037 does not decode, and the core refuses it with `UndraRestoreError.badSnapshot` (code 5). A snapshot from another build restores when its stores' types are unchanged or migrate by name; when they cannot, the restore is refused as a whole with `UndraRestoreError.incompatible` (code 7) and the core's ERROR log names the store and the reason.
* **Standard port records are public** (`HttpMethod`, `Header`, `HttpRequest`, `HttpResponse`, `HttpError`, `FsError`, `StorageError`, `NetKind`, `UndraAppState`, `UndraPanicFrame`, `UndraPanicReport`, `UndraBackgroundReport`; `Core/StandardRecords.swift`). Generated bindings refer to them and declare none (ADR-024, amended), and an app that answers `Http`, `Fs`, `Kv`, `SecureStore` or `Connectivity` itself builds and reads them: `try HttpRequest.undraDecoded(from: arguments)`, `HttpResponse(status: 200, headers: [...], body: ...).undraEncoded()`, `throw UndraPortError(body: StorageError.full.undraEncoded())`. They have the shape generated code would have: structs and enums that are `Sendable`, `Hashable` and `Codable` (the errors are `UndraError`s with the messages of the Rust `#[error]` attributes, and `LocalizedError`), public initializers (`HttpRequest` defaults headers, body and timeout to nothing), the names Kotlin and TypeScript use. Two spellings differ from Rust: `NetKind.disconnected` is `NetKind::None` (a case called `none` is ambiguous with `Optional.none` wherever the value is optional) and `AppState` is `UndraAppState`, the name it has had since v1 (an app's own `AppState` is the commonest type name in Swift). A module that declares a type with one of these names but another shape (an app record called `HttpRequest`) shadows the runtime's inside that module; code that imports both writes `ThatModule.HttpRequest` for the app's own. The wire layouts are asserted byte for byte in `StandardPortTests`, and `PublicStandardTypesTests` (a plain `import`, not `@testable`) fails to compile if any of it stops being public. Two of them are not fixed by docs/SPEC.md: `HttpMethod` (variants Get, Post, Put, Delete, Patch, Head, Options with indices 0 to 6) and the `NetKind` / `AppState` index order (the order the specification lists them). `undra-ports` must use the same, and its `encoding` test (`swift_numbering_matches`) reads `StandardRecords.swift` to check every index, `FsError`'s `Full` (3) and `Unavailable` (4) and `StorageError`'s five (`Unavailable` 0, `Full` 1, `Locked` 2, `Corrupt` 3, `Io` 4) included.
* **The opt-in ports: WebSocket, Sse, Db (ADR-047, ADR-048).** A core built with the `websocket`, `sse` or `db` feature declares them; `Adapters.platformDefault` serves them with `URLSessionWebSocketAdapter` (`URLSessionWebSocketTask`, one session per connection, or tasks of the app's own session: see "Your own `URLSession`"), `URLSessionSseAdapter` (a `URLSession` data task whose delegate hands the body over in chunks, and `SseParser`, the HTML standard's algorithm; the task is suspended while events wait for the core, so a core that stops pulling stops the socket) and `SQLiteDbAdapter` (the SQLite3 C API, one serial `DispatchQueue` per database, files in `Application Support/<bundle id>/Undra/db/<name>.sqlite`, next to the `Kv` adapter's, or a directory you pass). Registering them for a core without the features is harmless. To use your own, implement the adapter interface and register its binding: `Adapters.platformDefault.replacing(WebSocketPortAdapter(MyWebSocket()))` (likewise `SsePortAdapter`, `DbPortAdapter(_:busyTimeoutMs:)`). The bindings own what the core sees: ids from 1 never reused; for WebSocket and Sse a pump that reads ahead at most the `max` of the core's latest pull (16 before the first), so a core that stops reading stops the socket and TCP pushes back; a pull that answers up to `max` items, a burst still arriving as one answer (it waits until `max` are there, 2 ms without a new item, or 8 ms after the first), `[]` after the core's close, the stream's typed end otherwise; closes with 1001 when the core shuts down. For Db: name and version checks, `foreign_keys`, `busy_timeout`, WAL, all pending migrations in one transaction, one operation at a time per database, transactions that make the database's other statements wait at most the busy timeout (then `DbError.busy`). Every failure is typed by code, never by text: the URLSession adapter maps a refused upgrade to `WsError.refused(status:)`, the peer's close frame to `.closed(code:reason:)`, a frame that breaks RFC 6455 (Network.framework's `EPROTO`) to `.protocol`, anything else to `.network`; the SQLite adapter maps extended result codes (`CONSTRAINT_UNIQUE` 2067 and `PRIMARYKEY` 1555 → `.constraint(.unique)`, ...). Their records are public, `Sendable`, `Hashable` and `Codable` (the enums with data synthesize theirs), with the Rust `#[error]` texts as messages. `RealtimeAdapterTests` runs the default adapters against `contract-tests/servers/realtime-server.mjs` (Node; skipped without it unless `UNDRA_REQUIRE_TOOLCHAINS=1`), `SQLiteDbAdapterTests` the SQLite adapter in a temporary directory.
* **Frame-coalesced delivery (ADR-031).** A drain folds what it applies per signal: the last full value supersedes everything before it, the keyed patches after it are concatenated into one (count = the sum, ops in arrival order), and a lazy invalidation supersedes only the earlier invalidations of its signal, never the full value that carries the lazy list's page server (ADR-043: `[Full(handle), Inv, Inv]` is delivered as `[Full(handle), Inv(last)]`, `[Inv, Full]` as `[Full]`), so a signal is applied at most twice per drain in practice (a signal is a keyed list or a lazy list). Drains run from a `CADisplayLink` on iOS, tvOS and visionOS (paused while nothing waits) and on the main actor's next turn on macOS. Replies are never delayed: before a call's continuation resumes, an immediate drain is queued on the main queue, so code after `await store.method()` on the main actor sees the call's effects; `callSync`, `construct` and `observe` called on the main thread drain before they return. The queue holds at most `LoadOptions.maxPendingEntries` entries / `maxPendingBytes` bytes (65,536 / 16 MiB by default) before the delivering thread folds it in place; a signal whose merged patch passes 4,096 ops or 1 MiB is dropped there and re-observed by the next drain. Signals declared `#[undra(no_coalesce)]` (`UndraStore.init(core:handle:noCoalesce:)`) are applied entry by entry; SwiftUI still renders once per frame. `mirror.stats()` / `UndraStats.mirror` count change-sets, entries, applies, drains, compactions and resyncs; `mirror.addDrainListener` reports each drain.
* **`Mirror`** keeps the name the specification gives it, which shadows `Swift.Mirror` in files that import `UndraRuntime`; write `Swift.Mirror(reflecting:)` there.
* **Host-side memory for sync port replies** follows the rule written next to `undra_port_cb` in `include/undra.h`: `malloc`, `cap = 0`, the core copies and `free`s.

## Running the tests

On a Mac with Xcode 16 or newer (Swift 6.0):

```sh
cd runtimes/swift/UndraRuntime
swift test
```

Neither the Rust core nor an XCFramework is needed: the package links no core (see "The FFI target and the cores" below). The suites cover:

* the wire layer: shared contract vectors, round trips for every codec (empty, extremes, unicode), exact encodings of every payload, malformed input for every `WireError` case, byte-fuzz;
* loading an in-process core through a fake `UndraApi` table built in Swift from `@convention(c)` closures (`FakeCoreTable`; `InprocTransportTests`, `UndraCoreEntryTests`): the ABI version read first (versions 0, 1 and 3 refused before anything else in the table is read), a short table, a null entry or a missing namespace refused, the schema hash compared before `init`, one core per namespace and two namespaces side by side, load after shutdown, every operation and `buf_free` through the table, a port callback answered with `malloc`'d memory, and the entry (`UndraCoreEntry`: the table and hash filled in, its `core` the closed placeholder until loaded and after shutdown);
* the runtime core against a scripted in-process core (`FakeTransport`, which speaks the wire through the same `UndraTransport` seam the real transports use): sync and async calls, every reply status, rejection, cancellation (before, during and after the send), shutdown and disconnect, streams and their credit window (never more than 16 ahead, top-up at 8), stream errors (the stream's own `E`, and failed items that end as cancelled by the core, panicked or refused, through the raw and the generated mapping, with undecodable bodies) and early ending, constructors, the mirror (initial values before `observe` returns, ordering across threads, batching, malformed change-sets, re-entrant re-observe) and its coalesced delivery (`CoalesceTests`: the merge rules, a 600-history property test against sequential application, a 1,000,000-change-set backlog, read-your-writes, frame scheduling, counters, drain listeners, `no_coalesce`), stores and objects (register, close, deinit backstop, exactly one release), ports (sync, async, typed errors, unknown methods, log routing, the ERROR record of an untyped throw), events, timers, snapshots (layout 2, and the named restore codes) and statistics;
* the remote protocol (handshake, routing, schema check, port replies, sequence numbers) through a frame sink, and a whole `UndraCore` over it;
* the default stores of two cores in one process (`NamespaceStorageTests`): per namespace for Kv, Fs, Db and the Keychain service, one adapter value serving two cores, an adapter with a directory or service of its own untouched;
* the adapters: Clock, Rng, Log, Timer, Kv (files), SecureStore (over an in-memory backend, and the Keychain backend over a scripted Keychain; the real Keychain is exercised on a device), Fs, Http (over a `URLProtocol` stub), Connectivity classification and Lifecycle events;
* production operations (ADR-046): the panic and background records byte for byte against the Rust vectors (`DiagnosticsTests`), the `Diagnostics` port (main-thread, ordered, one per report, malformed arguments logged), `runInBackground` over the fake core (`BackgroundRunTests`: encoding, cancellation, failures), the scheduling decisions of `UndraBackground` against fakes of the OS (`BackgroundEngineTests`), and the default lifecycle reports (`LifecycleAdapterTests`);
* storage failures (`StorageFailureTests`, ADR-049): every `StorageError` from every `Kv` and `SecureStore` method answers port status 1 with the encoded error, byte for byte, through the real bridge; a failed write changes nothing and the next one succeeds; `Fs` answers a full disk with `FsError.full`; an untyped throw answers status 2 with an ERROR record; and the Foundation errors and Keychain statuses map as the ADR's table says.
* the opt-in ports (ADR-047, ADR-048): the twelve records byte for byte (`PortRecordsV2Tests`, the vectors of the Rust unit tests) and from outside the module (`PublicPortTypesTests`); the three bindings over scripted adapters (`WebSocketBindingTests`, `SseBindingTests`, `DbBindingTests`: ids, the read-ahead window, one pending pull, bursts, close and terminal semantics, unknown ids, the shutdown close; migrations, transactions, busy, ordering); the SSE parser (`SseParserTests`); the default adapters against `contract-tests/servers/realtime-server.mjs` (`RealtimeAdapterTests`: echo, subprotocol and headers, a refused upgrade with its status, the peer's close code, an abrupt drop, invalid UTF-8, a flood under a stalled reader, the SSE feed and its resume, refusals, the wrong content type, a hanging stream closed) and SQLite in a temporary directory (`SQLiteDbAdapterTests`: each constraint kind, busy, a corrupt file, typed cells, the one-statement rule, parameter counts, persistence).

`Tests/UndraRuntimeTests/Resources/wire-vectors.json` is a copy of `contract-tests/wire-vectors.json` (a SwiftPM package cannot read files outside its directory). After the contract changes, refresh it and commit the result:

```sh
runtimes/swift/scripts/sync-vectors.sh          # copy
runtimes/swift/scripts/sync-vectors.sh --check  # exit 1 if the copy is stale (for CI)
```

The tests use `@testable import UndraRuntime`, so run them in a debug configuration (the default).

## Using the wire layer

```swift
import UndraRuntime

// Encode.
var w = UndraWriter()
w.writeU32(7)
w.writeString("Milk")
w.writeBool(false)
let bytes = w.finish()

// Decode. Every failure is a WireError; nothing traps on hostile bytes.
var r = UndraReader(bytes)
let id = try r.readU32()
let title = try r.readString()
let done = try r.readBool()
try r.finish()                                   // WireError.trailingBytes if anything is left

// Codecs compose: [String: [Int32?]] is an UndraCodec.
let map: [String: [Int32?]] = ["b": [1, nil], "a": []]
let encoded = map.undraEncoded()                  // map keys are sorted by their encoded bytes
let decoded = try [String: [Int32?]].undraDecoded(from: encoded)

// Envelope and payloads.
var args = UndraWriter()
args.writeI32(2)
args.writeI32(3)
let call = Wire.Call(target: .objectMethod(handle: handle, methodId: fnv1a32("Calculator.add")),
                     callId: 9, args: args.finishSlice())
let frame = encodeEnvelope(kind: .call, seq: 1, schemaHash: schemaHash, payload: call.encode())

let (kind, seq, hash, payload) = try decodeEnvelope(frame)
try Envelope.requireSchemaHash(hash, expected: schemaHash)   // WireError.schemaMismatch

// Change-sets are walked without copying values.
try Wire.ChangeSet.forEachEntry(slice: payload) { handle, signalId, op, value in
    // `value` is an UndraReader restricted to this entry's bytes.
    let todos = try [Todo].undraDecode(&value)
}
```

### Naming

The Swift names mirror the Rust ones (`Writer::write_u32` is `UndraWriter.writeU32`):

| Rust | Swift |
|---|---|
| `write_u8` .. `write_f64`, `write_bool`, `write_str`, `write_bytes`, `write_len`, `into_vec` | `writeU8` .. `writeF64`, `writeBool`, `writeString`, `writeBytes`, `writeLen`, `finish` |
| `read_*`, `remaining`, `finish` | `readU8` .. `readF64`, `readBool`, `readString`, `readBytes`, `readLen`, `remaining`, `finish` |
| `Encode` / `Decode` | `UndraCodec` (`undraEncode(_:)` / `undraDecode(_:)`) |

### Byte fields are slices

Payload fields that hold a nested, already-encoded value (`Wire.Call.args`, `Wire.Reply.body`, `Wire.StreamItem.body`, `Wire.PortCall.args`, `Wire.Event.payload`, `Wire.ChangeEntry.value`, `Wire.SnapshotSignal.value`) are `ArraySlice<UInt8>` that share the input buffer, so decoding a message copies nothing. Build one with `UndraWriter.finishSlice()`, read one with `UndraReader(slice:)`. `UndraReader(_: [UInt8])` and `UndraReader(slice:)` are O(1); `UndraReader(copying:)` copies, and is the one to use for the `(ptr, len)` pair of a C callback, which is only valid until the callback returns.

### Behaviours worth knowing

* `readLen()` checks that a count does not exceed the bytes that remain, so a hostile `u32` cannot make a decoder allocate gigabytes. The consequence is that an array of zero-width elements (`[UndraUnit]`) with a non-zero count is rejected. No schema type produces one.
* `readString()` validates UTF-8 exactly (no overlong forms, no surrogates, nothing above U+10FFFF), and neither the reader nor the writer normalises: `"e\u{301}"` and `"\u{E9}"` are equal Swift strings but stay distinct on the wire.
* `WireError.unexpectedEOF(needed:at:)`: `needed` is the width of the read that failed, `at` its offset. Offsets are measured from the start of the buffer the reader was created over; sub-readers keep that origin.
* Map keys are sorted by their **encoded bytes** (unsigned, lexicographic), not by value: `"b"` sorts before `"aa"` because the length prefix comes first. The decoder rejects duplicate keys but accepts any order.
* `UndraDuration` decoding rejects negative values (`WireError.negativeDuration`); encoding writes the value as is and the core rejects it. Values converted from a `Swift.Duration` beyond the `Int64` nanosecond range saturate. `UndraDuration` stores the nanoseconds, so it exists on iOS 15; its `Duration` conversions and `extension Duration: UndraCodec` are `@available(iOS 16, macOS 13, *)`.
* `PatchOp.move(from:to:)` removes the element at `from` and re-inserts it so that it ends up at index `to`. `applyPatch` validates every op against the simulated list length first, so a failing patch throws `PatchError` and leaves the list untouched.
* The decoders reserve at most 65,536 elements up front regardless of the claimed count.

## The FFI target and the cores

`Sources/UndraFFI` is a header-only C target: `include/undra.h` declares the types of the native ABI of docs/SPEC.md section 6, version 2 (the `UndraApi` table, `UndraBuf` and the callback types) and no function, and `include/module.modulemap` exposes them to Swift as the `UndraFFI` module. `undra.c` is the one source file SwiftPM needs to build a C target; it only includes the header. The runtime therefore references no symbol of any core, and the package builds, tests and links without one. `undra.h` is byte-identical to `runtimes/rn/@undra/react-native/cpp/undra.h` and must stay in step with `UndraApi` in `crates/undra-ffi`; changing it is a boundary change and needs an ADR first (CLAUDE.md, R11).

Each core is its own image with one exported function, `const UndraApi *<namespace>_undra_api(void)` (ADR-044). `undra build` ships it in `<Namespace>Core.xcframework` (`lib<namespace>.a`, prelinked so that only that symbol is global; no `-force_load` needed), and `undra-bindgen` generates, next to the core's bindings, the C module `<Namespace>CoreFFI` that declares the function and the entry `Undra<Namespace>`, which hands it to an `UndraCoreEntry`:

```swift
// Generated: Sources/PlaygroundCore/Generated/Core.swift
public enum UndraPlaygroundCore {
    public static let namespace = "playground_core"
    private static let entry = UndraCoreEntry(namespace: namespace, schemaHash: UndraIds.schemaHash, api: { playground_core_undra_api() })
    public static func load(_ options: LoadOptions = .inproc()) throws -> UndraCore { try entry.load(options) }
    public static var core: UndraCore { entry.core }
}
```

An in-process load (`Core/CoreTable.swift`, the only file besides `Core/InprocTransport.swift` that imports `UndraFFI`) reads `abi_version` first and refuses any other version than 2 with `UndraLoadError.abiMismatch` before it reads anything else, then checks `size` (at least a version 2 `UndraApi`; fields are only ever appended), the namespace and the 17 entries (`UndraLoadError.invalidCoreTable`), copies the entries out once, and compares `schema_hash` with the bindings' before `init` runs (`UndraSchemaMismatchError`). One core of a namespace runs at a time (`UndraLoadError.alreadyLoaded`); cores of different namespaces, two independent SDKs' cores in one app, load side by side, each with its own `UndraCore`. `UndraCore.load` called directly needs `api` and `expectedSchemaHash` in its options (`UndraLoadError.missingCoreTable`, `.missingSchemaHash`); the generated entry fills both in. A namespace that is not a core's (`LoadOptions.namespace`, or the table's: it names the default stores' directories) is refused with `UndraLoadError.invalidNamespace` before anything starts. `UndraCore.shared` remains the first core loaded, for app code; generated code uses its own entry's `core`.

`crates/undra-ffi/tests/swift/run.sh` runs this runtime over the real C ABI against the fixture core (`undra_fixture`), and `contract-tests/swift` runs the contract scenarios over the playground core.
