//
// Copyright 2020 Signal Messenger, LLC.
// SPDX-License-Identifier: AGPL-3.0-only
//

use std::any::Any;
use std::cell::{Ref, RefCell};
use std::panic::UnwindSafe;

// See https://github.com/rust-lang/rfcs/issues/1389
pub(crate) fn describe_any(any: &Box<dyn Any + Send>) -> String {
    if let Some(msg) = any.downcast_ref::<&str>() {
        msg.to_string()
    } else if let Some(msg) = any.downcast_ref::<String>() {
        msg.to_string()
    } else {
        "(break on rust_panic to debug)".to_string()
    }
}

/// Like [RefCell], but requires that mutating updates be [UnwindSafe].
///
/// This is more a reminder to not do any non-trivial work during an update.
/// The cell should always be in a valid state *or*
/// you should guarantee execution cannot resume after a panic when mutating the cell.
///
/// It's not called UnwindSafeRefCell because it can't make that guarantee;
/// it's only a reminder.
pub(crate) struct UnwindSafetyRefCell<T> {
    cell: RefCell<T>,
}

impl<T> UnwindSafetyRefCell<T> {
    /// Creates a new cell containing `value`.
    pub(crate) fn new(value: T) -> Self {
        Self {
            cell: RefCell::new(value),
        }
    }

    /// Immutably borrows the wrapped value.
    ///
    /// Equivalent to [RefCell::borrow].
    pub(crate) fn borrow(&self) -> Ref<T> {
        self.cell.borrow()
    }

    /// Mutates the wrapped value.
    ///
    /// Roughly equivalent to [RefCell::borrow_mut], but with the added restriction
    /// of updates being UnwindSafe. This is more a reminder to not leave the value in an inconsistent state;
    /// it's not the *captures* that need unwind safety so much as the value being mutated.
    pub(crate) fn update<R>(&self, f: impl FnOnce(&mut T) -> R + UnwindSafe) -> R {
        f(&mut self.cell.borrow_mut())
    }

    /// Gets the address of the cell, opaquely.
    ///
    /// Used in this crate for a unique ID. Not intended to be generally useful.
    pub(crate) fn as_address(&self) -> *const () {
        self.cell.as_ptr() as *const ()
    }
}
