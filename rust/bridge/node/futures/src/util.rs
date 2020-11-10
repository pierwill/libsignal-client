//
// Copyright 2020 Signal Messenger, LLC.
// SPDX-License-Identifier: AGPL-3.0-only
//

use neon::prelude::*;
use std::any::Any;
use std::panic::AssertUnwindSafe;

pub(crate) fn panic_on_exceptions<'a, R>(
    cx: &mut FunctionContext<'a>,
    action: impl FnOnce(&mut FunctionContext<'a>) -> R,
) -> R {
    let mut result = None;
    {
        let mut result = AssertUnwindSafe(&mut result);
        let action = AssertUnwindSafe(action);
        let maybe_exception = cx.try_catch(move |cx| {
            let AssertUnwindSafe(action) = action;
            **result = Some(action(cx));
            // Dummy return value; see https://github.com/neon-bindings/neon/issues/630
            Ok(cx.undefined())
        });
        maybe_exception.unwrap_or_else(|exception| {
            let msg = exception
                .to_string(cx)
                .map_or_else(|_| "unknown".into(), |msg| msg.value());
            panic!("Unhandled JavaScript exception: {}", msg)
        });
    }
    result.unwrap()
}

// See https://github.com/rust-lang/rfcs/issues/1389
pub(crate) fn describe_any(any: &Box<dyn Any + Send>) -> String {
    if let Some(msg) = any.downcast_ref::<&str>() {
        msg.to_string()
    } else if let Some(msg) = any.downcast_ref::<String>() {
        msg.to_string()
    } else {
        "(break on begin_panic to debug)".to_string()
    }
}
