//! Incremental lattice cache used by `InputSession`.
//!
//! The cache keeps a single `Arc<Lattice>` keyed by the current reading so
//! consecutive keystrokes can extend the existing lattice instead of rebuilding
//! from scratch. When the reading no longer matches (backspace, auto-commit,
//! prefix mismatch) the cache falls back to a fresh build.

use std::sync::Arc;

use lex_core::converter::{build_lattice, ConversionContext, Lattice};

pub(crate) struct LatticeCache {
    lattice: Option<Arc<Lattice>>,
}

impl LatticeCache {
    pub(crate) fn new() -> Self {
        Self { lattice: None }
    }

    /// Drop any cached lattice (called on backspace, auto-commit, etc.).
    pub(crate) fn invalidate(&mut self) {
        self.lattice = None;
    }

    /// Return a lattice for `reading`, extending the cached one when possible.
    ///
    /// Reuses the cached lattice unchanged when `reading` matches, extends it
    /// when `reading` is a pure suffix append, and rebuilds otherwise.
    pub(crate) fn get_or_build(
        &mut self,
        reading: &str,
        ctx: &ConversionContext<'_>,
    ) -> Arc<Lattice> {
        if let Some(arc) = self.lattice.take() {
            if reading == arc.input {
                self.lattice = Some(Arc::clone(&arc));
                return arc;
            }
            if reading.starts_with(&arc.input) {
                let mut owned = Arc::try_unwrap(arc).unwrap_or_else(|shared| (*shared).clone());
                owned.extend(ctx.dict, reading);
                let arc = Arc::new(owned);
                self.lattice = Some(Arc::clone(&arc));
                return arc;
            }
        }
        let arc = Arc::new(build_lattice(ctx.dict, reading));
        self.lattice = Some(Arc::clone(&arc));
        arc
    }
}
