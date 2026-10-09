//! The Undra cookbook: one recipe per module, each a small piece of a real core.
//!
//! Every Rust sample on the docs site's cookbook pages is code from this crate, and every recipe has
//! tests that run it against the test runtime and the deterministic fakes (`undra::ports::fakes`), so
//! a recipe is checked by the compiler and by `cargo test -p cookbook`, not only read. It is written
//! the way an application writes its core, with nothing but the public `undra` API (constitution
//! R10), and like every core it reads no clock and no random source and starts no thread (R12): time
//! is the `Clock` and `Timer` ports, the network is the `Http` port, secrets are the `SecureStore`
//! port, files are the `Fs` port.
//!
//! | Module | Recipe |
//! |---|---|
//! | [`net`] | where the server is and what a failed request is (shared by the others) |
//! | [`auth`] | a session store, tokens in `SecureStore`, a 401 that re-authenticates, a logout that clears state |
//! | [`paging`] | a keyed list fed page by page, a derived view, the cursor in a signal |
//! | [`forms`] | a signal per field, the errors derived from them, a command that refuses typed |
//! | [`upload`] | a file from `Fs` in parts over `Http`, progress as a signal, retries through the offline queue |
//! | [`offline`] | persisted queries, writes that queue, an outbox, and an update (`#[undra(default)]`, `#[undra::migrate]`) |
//! | [`leaderboard`] | a 100,000-player ranking kept off the core: ingested on the blocking pool, published as a window of 60 rows in one transaction ([`standings`] is the structure) |
//! | [`haptics`] | a port of the app's own (`#[undra::port]`), implemented and registered on each platform (the site's "Your own port" recipe) |
//! | `realtime` | a WebSocket reconnecting in the core with backoff, server-sent events as the fallback (feature `realtime`: it needs the opt-in `WebSocket` and `Sse` ports) |

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod auth;
pub mod forms;
pub mod haptics;
pub mod leaderboard;
pub mod net;
pub mod offline;
pub mod paging;
#[cfg(feature = "realtime")]
pub mod realtime;
pub mod standings;
#[cfg(test)]
mod standings_tests;
pub mod upload;

pub use auth::{Auth, AuthError, Profile, ProfileQuery, Session, authed, profile};
pub use forms::{Field, FieldError, SignUp, SubmitError};
pub use haptics::{Haptics, confirm};
pub use leaderboard::{AROUND, Delta, Leaderboard, LeaderboardError, Row, Summary, TOP};
pub use net::{NetError, ServerConfig, configure_server};
pub use offline::{
    AddNoteMutation, Note, NotesQuery, Outbox, Stuck, add_note, create_note, discard_stuck, notes,
    outbox, retry_stuck,
};
pub use paging::{Feed, Post};
#[cfg(feature = "realtime")]
pub use realtime::{Link, Live, LiveError, Message};
pub use upload::{
    CompleteUploadMutation, PutPartMutation, Upload, UploadError, UploadState, Uploads,
    complete_upload, put_part,
};
