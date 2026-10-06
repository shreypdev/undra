// The Kotlin lines the cookbook pages show. The `docs:begin` / `docs:end` markers delimit what a page
// quotes (site/scripts/build-cookbook.mjs copies it). They are plain Kotlin over the generated
// bindings, so `../check.sh` compiles them with kotlinc; in Compose a `StateFlow` is read with
// `collectAsState()`.
package dev.undra.cookbook.snippets

import dev.undra.cookbook.core.*
import dev.undra.runtime.UndraException
import kotlinx.coroutines.flow.StateFlow

// docs:begin auth-kotlin
fun label(session: Session): String = when (session) {
    Session.SignedOut -> "Sign in"
    is Session.SignedIn -> session.user
}

suspend fun logIn(auth: Auth, email: String, password: String): String? =
    try {
        auth.signIn(email, password)
        null
    } catch (e: AuthError.BadCredentials) {
        "Wrong email or password"
    } catch (e: UndraException) {
        e.message
    }
// docs:end

// docs:begin paging-kotlin
class FeedScreen(private val feed: Feed) {
    val rows: StateFlow<List<Post>> = feed.visible

    // Call it when the last row scrolls into view: the core ignores a call while a page is in
    // flight and after the last page.
    suspend fun onEndReached() { feed.loadMore() }

    fun onSearch(text: String) = feed.setTopic(text)
}
// docs:end

// docs:begin forms-kotlin
class SignUpScreen(private val form: SignUp) {
    val errors: StateFlow<List<FieldError>> = form.errors
    val canSubmit: StateFlow<Boolean> = form.valid

    fun onEmail(text: String) = form.setEmail(text)
    fun onEmailLeft() = form.blur(Field.EMAIL)

    suspend fun create(): String? =
        try {
            form.submit()
            null
        } catch (e: SubmitError.EmailTaken) {
            "That email is already registered"
        } catch (e: SubmitError.Invalid) {
            null // `errors` already says why
        }
}
// docs:end

// docs:begin upload-kotlin
class UploadsScreen(private val uploads: Uploads) {
    val rows: StateFlow<List<Upload>> = uploads.uploads

    fun progress(row: Upload): Float = row.sent.toFloat() / maxOf(row.total, 1u).toFloat()

    suspend fun retry(row: Upload) = uploads.retry(row.id)
}
// docs:end

// docs:begin offline-kotlin
suspend fun notesScreen() {
    val notes = NotesQueryHandle.create("inbox")      // cached on disk: shown before the network answers
    notes.data

    // Shows at once; returns when the server has it, even if that is after the tunnel.
    createNote(list = "inbox", text = "Buy milk", pinned = false)

    val waiting = outbox().pending             // "2 changes waiting to sync"
    println(waiting)
}
// docs:end

// docs:begin leaderboard-kotlin
class LeaderboardScreen(private val board: Leaderboard, private val myId: UInt) {
    // Fifty rows, up to seven rows around me and four numbers: the 100,000 players stay in the core.
    val top: StateFlow<List<Row>> = board.top
    val around: StateFlow<List<Row>> = board.around
    val summary: StateFlow<Summary> = board.summary

    suspend fun refresh() { board.loadSnapshot("/standings") }

    suspend fun findMe() = board.follow(myId)

    fun isMine(row: Row): Boolean = row.id == myId
}
// docs:end
