//
// Copyright 2020 Signal Messenger, LLC.
// SPDX-License-Identifier: AGPL-3.0-only
//

use futures::FutureExt;
use neon::prelude::*;
use std::future::Future;
use std::panic::{catch_unwind, AssertUnwindSafe};

use crate::util::describe_any;
use crate::*;

fn save_promise_callbacks(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.this();

    let resolve = cx.argument::<JsFunction>(0)?;
    this.set(&mut cx, "_resolve", resolve)?;

    let reject = cx.argument::<JsFunction>(1)?;
    this.set(&mut cx, "_reject", reject)?;

    Ok(cx.undefined())
}

pub fn promise<'a, T, F, R>(
    cx: &mut FunctionContext<'a>,
    callback: impl FnOnce(&mut FunctionContext<'a>, JsAsyncContext) -> R,
) -> JsResult<'a, JsObject>
where
    T: neon::types::Value,
    F: for<'b> FnOnce(&mut FunctionContext<'b>) -> JsResult<'b, T>,
    R: Future<Output = Result<F, JsAsyncContextKey<JsValue>>> + 'static,
{
    let future_context = JsAsyncContext::new();
    let future = callback(cx, future_context.clone());

    let object = cx.empty_object();
    let save_promise_callbacks_fn = JsFunction::new(cx, save_promise_callbacks)?;
    let bind_fn: Handle<JsFunction> = save_promise_callbacks_fn
        .get(cx, "bind")?
        .downcast_or_throw(cx)?;
    let bound_save_promise_callbacks = bind_fn.call(cx, save_promise_callbacks_fn, vec![object])?;

    let promise_ctor: Handle<JsFunction> = cx.global().get(cx, "Promise")?.downcast_or_throw(cx)?;
    let promise = promise_ctor.construct(cx, vec![bound_save_promise_callbacks])?;

    let promise_object_key = future_context.register_context_data(cx, object);

    future_context.clone().run(cx, async move {
        let result = AssertUnwindSafe(future).catch_unwind().await;

        // Any JavaScript errors that happen here (as opposed to in the result callback)
        // are beyond our ability to recover.
        // Ignore them and let JsAsyncContext complain about the uncaught errors.
        let _ = future_context.with_context(|mut cx| -> NeonResult<()> {
            let resolved_result = match result {
                Ok(Ok(resolve)) => {
                    let resolve = AssertUnwindSafe(resolve);
                    let mut cx = AssertUnwindSafe(&mut cx);
                    catch_unwind(move || cx.try_catch(|cx| resolve.0(cx)))
                }
                Ok(Err(saved_error)) => Ok(Err(future_context.get_context_data(cx, saved_error))),
                Err(panic) => Err(panic),
            };
            let folded_result = resolved_result.unwrap_or_else(|panic| {
                Err(cx
                    .error(format!("unexpected panic: {}", describe_any(&panic)))
                    .expect("can create an Error")
                    .upcast())
            });

            let (value, callback_name) = match folded_result {
                Ok(value) => (value.upcast(), "_resolve"),
                Err(exception) => (exception, "_reject"),
            };

            let promise = future_context.get_context_data(cx, promise_object_key);
            let promise_callback: Handle<JsFunction> =
                promise.get(cx, callback_name)?.downcast_or_throw(cx)?;
            promise_callback.call(cx, promise, vec![value])?;
            Ok(())
        });
    });

    Ok(promise)
}

// Works around a bug in the Rust compiler by providing extra type information.
pub fn fulfill_promise<C, T, E>(callback: C) -> Result<C, E>
where
    T: Value,
    C: for<'a> FnOnce(&mut FunctionContext<'a>) -> JsResult<'a, T>,
{
    Ok(callback)
}
