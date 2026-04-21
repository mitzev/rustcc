//! Explicit C++ move-ctor invocation marker.
//!
//! See `docs/ownership_and_safety.md §4.2`.

use crate::owned::CxxOwned;

pub struct CxxMove<T> {
    owned: CxxOwned<T>,
}

impl<T> CxxMove<T> {
    pub fn from(owned: CxxOwned<T>) -> Self {
        Self { owned }
    }

    pub(crate) fn into_owned(self) -> CxxOwned<T> {
        self.owned
    }
}
