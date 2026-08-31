//! UI-aware helpers: things that drive signals and spawn work on behalf of a
//! screen, rather than talking to Azure or the disk.
//!
//! Kept out of `services` on purpose. Everything under there is pure — it
//! shells out, parses, returns — which is what makes it testable without a
//! renderer. A helper that holds a `Signal<bool>` true while it waits is a
//! different kind of thing, and mixing the two made `services` quietly depend
//! on Dioxus.
pub mod signin;
