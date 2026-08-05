//! Data types and fetcher for the rimeterm ModelsPane.
//!
//! Ported from [`reyamira/models`](https://github.com/reyamira/models)
//! (MIT-licensed, `modelsdev` v0.14.0). We rebuild only the Models tab
//! in-process rather than shelling out to the `models` binary, so users
//! don't need a separate `cargo install modelsdev` to get the
//! [`ModelsPane`] view.
//!
//! Attribution note: `data.rs` mirrors upstream `src/data.rs` field
//! shapes 1:1 so a schema change on `models.dev` translates directly.
//! Bump the version reference in the module doc when re-syncing.
//!
//! Unlike upstream — which uses async `reqwest` inside a tokio runtime —
//! this crate exposes a blocking `fetch_providers()` so the pane worker
//! thread (an `std::thread`, matching `AgtopWorker`) can call it
//! directly without dragging async plumbing into the pane.
//!
//! [`ModelsPane`]: ../rimeterm_tui/models_pane/struct.ModelsPane.html

pub mod api;
pub mod data;
pub mod format;

pub use api::{FetchError, fetch_providers, fetch_providers_from};
pub use data::{
    Cost, CostTier, Limits, Modalities, Model, Provider, ProvidersMap, ReasoningOption, TierSpec,
};
