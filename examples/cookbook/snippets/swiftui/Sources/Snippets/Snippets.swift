// The Swift lines the cookbook pages show. The `docs:begin` / `docs:end` markers delimit what a page
// quotes (site/scripts/build-cookbook.mjs copies it); everything outside them is scaffolding that
// makes the lines compile (`../../check.sh`).
import CookbookCore
import SwiftUI
import UndraRuntime

// MARK: scaffolding

struct LoginView: View { let auth: Auth; var body: some View { EmptyView() } }
struct HomeView: View { let user: String; var body: some View { EmptyView() } }

// MARK: auth

// docs:begin auth-swift
struct Root: View {
    let auth: Auth

    var body: some View {
        switch auth.session {
        case .signedOut: LoginView(auth: auth)
        case .signedIn(let user): HomeView(user: user)
        }
    }
}

@MainActor func logIn(_ auth: Auth, email: String, password: String) async -> String? {
    do {
        try await auth.signIn(email: email, password: password)
        return nil
    } catch AuthError.badCredentials {
        return "Wrong email or password"
    } catch {
        return error.localizedDescription
    }
}
// docs:end

// MARK: paging

// docs:begin paging-swift
struct FeedView: View {
    let feed: Feed

    var body: some View {
        List(feed.visible, id: \.id) { post in
            Text(post.title)
                .task {
                    // The core ignores a call while a page is in flight or after the last page.
                    if post.id == feed.visible.last?.id { _ = try? await feed.loadMore() }
                }
        }
        .searchable(text: Binding(get: { feed.topic }, set: { feed.setTopic(topic: $0) }))
        .refreshable { _ = try? await feed.refresh() }
    }
}
// docs:end

// MARK: forms

// docs:begin forms-swift
struct SignUpView: View {
    let form: SignUp

    var body: some View {
        Form {
            TextField("Email", text: Binding(get: { form.email }, set: { form.setEmail(email: $0) }))
                .onSubmit { form.blur(.email) }
            ForEach(form.errors, id: \.message) { error in
                Text(error.message).foregroundStyle(.red)
            }
            Button("Create account") {
                Task {
                    do { _ = try await form.submit() }
                    catch SubmitError.emailTaken { /* point at the email field */ }
                    catch { /* Invalid: the errors above already say why */ }
                }
            }
            .disabled(!form.valid)
        }
    }
}
// docs:end

// MARK: upload

// docs:begin upload-swift
struct UploadsView: View {
    let uploads: Uploads

    var body: some View {
        ForEach(uploads.uploads, id: \.id) { row in
            ProgressView(value: Double(row.sent), total: Double(max(row.total, 1)))
            if case .failed(let reason) = row.state {
                Button("Retry (\(reason))") { Task { try? await uploads.retry(id: row.id) } }
            }
        }
    }
}
// docs:end

// MARK: offline

// docs:begin offline-swift
@MainActor func notesScreen() async throws {
    let notes = try NotesQueryHandle(list: "inbox")       // cached on disk: shown before the network answers
    _ = notes.data

    // Shows at once; returns when the server has it, even if that is after the train leaves the tunnel.
    _ = try await createNote(list: "inbox", text: "Buy milk", pinned: false)

    let waiting = try outbox().pending                    // "2 changes waiting to sync"
    _ = waiting
}
// docs:end

// MARK: leaderboard

// docs:begin leaderboard-swift
struct LeaderboardView: View {
    let board: Leaderboard
    let myId: UInt32

    var body: some View {
        List {
            // Four numbers about 100,000 players, and the best fifty of them.
            Section("\(board.summary.players) players, median score \(board.summary.medianScore)") {
                ForEach(board.top, id: \.id) { row in PlayerRow(row: row, mine: row.id == myId) }
            }
            // Where I stand: a few rows around me, which the core moves when I ask or when scores change.
            if let me = board.summary.me {
                Section("You are #\(me.rank)") {
                    ForEach(board.around, id: \.id) { row in PlayerRow(row: row, mine: row.id == myId) }
                }
            } else {
                Button("Find me") { Task { try? await board.follow(id: myId) } }
            }
        }
        .task { _ = try? await board.loadSnapshot(path: "/standings") }
        .refreshable { _ = try? await board.loadSnapshot(path: "/standings") }
    }
}

struct PlayerRow: View {
    let row: Row
    let mine: Bool

    var body: some View {
        HStack {
            Text("#\(row.rank)").monospacedDigit()
            Text("Player \(row.id)").fontWeight(mine ? .bold : .regular)
            Spacer()
            Text("\(row.score)").monospacedDigit()
        }
    }
}
// docs:end

// MARK: network

// docs:begin network-swift
/// The app's session delegate: pinning and authentication challenges are answered here, for every request the core makes.
final class AppSessionDelegate: NSObject, URLSessionDelegate, @unchecked Sendable {
    func urlSession(
        _ session: URLSession,
        didReceive challenge: URLAuthenticationChallenge
    ) async -> (URLSession.AuthChallengeDisposition, URLCredential?) {
        // Your pinning goes here: compare challenge.protectionSpace.serverTrust with the certificate you ship.
        return (.performDefaultHandling, nil)
    }
}

/// The ports that carry the core's traffic, over the app's own session. Event streams get a session of their own, with the
/// same delegate and a configuration that lets a stream stay quiet.
func appAdapters(configuration: URLSessionConfiguration, delegate: AppSessionDelegate) -> Adapters {
    configuration.httpAdditionalHeaders = ["X-App-Version": "1.4.2"]   // on everything the core sends
    let session = URLSession(configuration: configuration, delegate: delegate, delegateQueue: nil)

    let streaming = (configuration.copy() as? URLSessionConfiguration) ?? .default
    streaming.timeoutIntervalForRequest = 24 * 60 * 60
    streaming.httpMaximumConnectionsPerHost = 1_024
    let streams = URLSession(configuration: streaming, delegate: delegate, delegateQueue: nil)

    return Adapters.platformDefault
        .replacing(HttpAdapter(session: session))
        .replacing(URLSessionWebSocketAdapter(session: session))
        .replacing(URLSessionSseAdapter(session: streams))
}
// docs:end

// MARK: custom port

/// Scaffolding: the engine the app already has.
enum Haptic { static func play(_ strength: UInt8) {} }

// docs:begin custom-port-swift
// From closures: nothing conforms, and the form that compiles unchanged under default main-actor isolation
// (Xcode 26). The closure runs on the core's thread.
let haptics = hapticsPortImpl(tap: { strength in Haptic.play(strength) })

// Or a class. A class that conforms to a port protocol is nonisolated, since the core calls it from its own
// thread: it keeps its state in `let`s or behind a lock, and reads main-actor state from an async port only,
// with `await MainActor.run { .. }`.
final class AppHaptics: Haptics {
    func tap(strength: UInt8) { Haptic.play(strength) }
}

// Registered with the rest of the adapters, before the core loads: `load(.inproc(adapters: adapters))`.
let adapters = Adapters.platformDefault
    .replacing(PortImplAdapter(portId: UndraIds.Ports.Haptics.portId, impl: haptics))
// docs:end
