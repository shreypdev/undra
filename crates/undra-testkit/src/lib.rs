#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![doc = include_str!("../README.md")]
//!
//! # Where things are
//!
//! | Item | What |
//! |---|---|
//! | [`Recording`], [`Event`], [`EventKind`] | the `undra.recording` format (version 1): [`Recording::to_json`], [`Recording::from_json`] |
//! | [`Recorder`] | collects a recording from envelopes ([`Recorder::record_envelope`]) or from a host ([`Recorder::host`]) |
//! | [`Replayer`] | answers port calls from a recording, in order, with typed [`ReplayError`]s |
//! | [`Seed`] | the starting state of the fakes as JSON, shared with the platform kits |
//! | [`Harness`] | a `TestRuntime` with the fakes installed and a manual clock |
//! | [`conformance`] | the file every implementation of the fakes is checked against |
//! | [`decode`] | [`SchemaIndex`](decode::SchemaIndex): names and decodes a recording's ids and bytes from a schema |
//! | [`drift`] | [`Session`](drift::Session) and [`compare`](drift::compare): where two recordings of one flow diverged (`undra drift`) |
//!
//! The facade crate re-exports all of it as `undra::testing`, together with the test runtime
//! (`undra::runtime::testing`) and the fakes (`undra::ports::fakes`).

pub mod conformance;
pub mod decode;
pub mod drift;
mod harness;
mod hex;
mod names;
mod recorder;
mod recording;
mod replayer;
mod seed;

pub use harness::Harness;
pub use names::{standard_name, standard_port};
pub use recorder::{Recorder, RecordingClock, RecordingRng, RecordingTap};
#[doc(hidden)]
pub use recording::every_kind;
pub use recording::{Entry, Event, EventKind, FORMAT, Recording, RecordingError, Target, VERSION};
pub use replayer::{ArgsPolicy, ReplayError, Replayer};
pub use seed::{HttpRule, SEED_VERSION, Seed, SeedError};

pub use undra_ports::fakes;
pub use undra_runtime::testing::{RecordingHost, TestRuntime};
