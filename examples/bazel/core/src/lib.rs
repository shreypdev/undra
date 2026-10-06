//! The core of bazel-hello: the app's logic, written once in Rust.
//!
//! Everything marked `#[undra::api]` crosses into Swift, Kotlin and TypeScript. `undra bindgen`
//! reads this crate's schema and generates the bindings the app shells call, so the Rust source
//! here is the only place where the app's logic and its types are declared.
//!
//! * A *record* (`Todo`) crosses by value: a Swift struct, a Kotlin data class, a TypeScript
//!   interface.
//! * An *enum* (`Filter`) and an *error* (`TodoError`) cross by value too; errors become typed
//!   throws, sealed exceptions and error classes.
//! * A *store* (`Todos`) holds signals the UI observes and commands the UI calls. Reads never
//!   cross the boundary: the platforms keep a mirror of each signal and the core pushes changes.
//!
//! Try it: `undra dev` serves this core over a WebSocket so a running app talks to it while you
//! edit; `undra build` produces the libraries the apps link.

use std::sync::atomic::{AtomicU64, Ordering};

use undra::prelude::*;

/// One item of the to-do list.
#[undra::api]
#[derive(Clone, Debug, PartialEq)]
pub struct Todo {
    /// Identity of the item; the list is updated by key, so the UI diffs by it.
    pub id: Uuid,
    /// What has to be done.
    pub title: String,
    /// Whether it is finished.
    pub done: bool,
}

/// Which items the list shows.
#[undra::api]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Filter {
    /// Every item.
    All,
    /// Items that are not finished.
    Active,
    /// Items that are finished.
    Done,
}

impl Filter {
    fn matches(self, todo: &Todo) -> bool {
        match self {
            Filter::All => true,
            Filter::Active => !todo.done,
            Filter::Done => todo.done,
        }
    }
}

/// Why an item could not be added.
#[undra::error]
#[derive(Clone, Debug, PartialEq)]
pub enum TodoError {
    /// The title is empty once spaces are trimmed.
    #[error("the title cannot be empty")]
    EmptyTitle,
}

/// The to-do list: what the UI observes (`todos`, `filter`, `visible`, `remaining`) and calls.
#[undra::store(restore = "Self::assemble")]
pub struct Todos {
    next: AtomicU64,
    #[undra(key = "id")]
    todos: Signal<Vec<Todo>>,
    filter: Signal<Filter>,
    visible: Computed<Vec<Todo>>,
    remaining: Computed<u32>,
}

#[undra::api(store)]
impl Todos {
    /// An empty list showing every item.
    pub fn new(ctx: Ctx) -> Self {
        Self::assemble(ctx, Signal::new(vec![]), Signal::new(Filter::All))
    }

    // Used by `new` and, through `restore = ".."`, to rebuild the store from a snapshot.
    fn assemble(_ctx: Ctx, todos: Signal<Vec<Todo>>, filter: Signal<Filter>) -> Self {
        let visible = Computed::new((&todos, &filter), |(todos, filter)| {
            todos
                .iter()
                .filter(|todo| filter.matches(todo))
                .cloned()
                .collect()
        });
        let remaining = Computed::new(&todos, |todos| {
            todos.iter().filter(|todo| !todo.done).count() as u32
        });
        let next = AtomicU64::new(todos.with(|list| list.len() as u64) + 1);
        Self {
            next,
            todos,
            filter,
            visible,
            remaining,
        }
    }

    /// Adds an item at the end of the list.
    pub async fn add(&self, title: String) -> Result<Todo, TodoError> {
        let title = title.trim().to_owned();
        if title.is_empty() {
            return Err(TodoError::EmptyTitle);
        }
        // The core is deterministic (no clock, no randomness): identities come from a counter.
        let mut id = [0; 16];
        id[..8].copy_from_slice(&self.next.fetch_add(1, Ordering::Relaxed).to_be_bytes());
        let todo = Todo {
            id: Uuid(id),
            title,
            done: false,
        };
        self.todos.update(|list| list.push(todo.clone()));
        Ok(todo)
    }

    /// Flips the `done` flag of the item with `id`; unknown ids are ignored.
    pub fn toggle(&self, id: Uuid) {
        self.todos.update(|list| {
            if let Some(todo) = list.iter_mut().find(|todo| todo.id == id) {
                todo.done = !todo.done;
            }
        });
    }

    /// Chooses which items `visible` holds.
    pub fn set_filter(&self, filter: Filter) {
        self.filter.set(filter);
    }

    /// Removes every finished item.
    pub fn clear_done(&self) {
        self.todos.update(|list| list.retain(|todo| !todo.done));
    }
}

/// A greeting, to show a plain function crossing the boundary.
#[undra::api]
pub fn greeting(name: String) -> String {
    format!("Hello, {name}, from the bazel-hello core")
}

/// Panics with `reason`, on purpose: the crash that `//symbols:symbolicate_test` makes the Bazel-built core have, so a panic
/// report of that exact build can be resolved with its symbol files. It is test-only: nothing in the app calls it, and no
/// consumer under `kotlin/`, `swift/`, `ts/` or `consumer/` does either. The boundary turns the panic into a typed error and
/// the core reports it to the app's `onPanic` (ADR-046).
#[undra::api]
pub fn crash_for_symbols_test(reason: String) -> u32 {
    panic!("{reason}")
}

#[cfg(test)]
mod tests {
    use undra::runtime::testing::TestRuntime;
    use undra::wire::payload::{CallTarget, ReplyStatus};

    use super::*;

    #[test]
    fn the_store_is_constructible_through_the_runtime() {
        let core = TestRuntime::new();
        let target = CallTarget::Constructor {
            type_id: undra::meta::ids::type_id("Todos"),
            method_id: undra::meta::ids::method_id("Todos", "new"),
        };
        assert_eq!(core.call_sync(target, 1, &[]).status, ReplyStatus::Ok);
    }

    #[test]
    #[should_panic(expected = "on purpose")]
    fn the_test_only_crash_panics_with_its_reason() {
        crash_for_symbols_test("on purpose".to_owned());
    }

    #[test]
    fn filters_select_items() {
        let todo = |done| Todo {
            id: Uuid([0; 16]),
            title: "x".into(),
            done,
        };
        assert!(Filter::All.matches(&todo(true)) && Filter::All.matches(&todo(false)));
        assert!(Filter::Active.matches(&todo(false)) && !Filter::Active.matches(&todo(true)));
        assert!(Filter::Done.matches(&todo(true)) && !Filter::Done.matches(&todo(false)));
    }
}
