//
// Copyright 2020 Signal Messenger, LLC.
// SPDX-License-Identifier: AGPL-3.0-only
//

use std::any::Any;
use std::cell::RefCell;
use std::panic;

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

// Based on https://crates.io/crates/take_mut.
pub(crate) fn as_ref_cell<T, F>(mut_ref: &mut T, closure: F)
where
    F: FnOnce(&RefCell<T>),
{
    use std::ptr;

    unsafe {
        let mut cell = RefCell::new(ptr::read(mut_ref));
        let result = panic::catch_unwind(panic::AssertUnwindSafe(|| closure(&cell)));
        cell.undo_leak();
        ptr::write(mut_ref, cell.into_inner());
        if let Err(err) = result {
            panic::resume_unwind(err);
        }
    }
}
