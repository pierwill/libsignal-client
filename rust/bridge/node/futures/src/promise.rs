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

const RESOLVE_SLOT: &str = "_resolve";
const REJECT_SLOT: &str = "_reject";

/// A JavaScript-compatible function that saves its first two arguments as `this._resolve` and `this._reject`.
fn save_promise_callbacks(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.this();

    let resolve = cx.argument::<JsFunction>(0)?;
    this.set(&mut cx, RESOLVE_SLOT, resolve)?;

    let reject = cx.argument::<JsFunction>(1)?;
    this.set(&mut cx, REJECT_SLOT, reject)?;

    Ok(cx.undefined())
}

/// Produces a JavaScript Promise that represents the result of a Rust computation.
///
/// There's a lot going on here, so let's break it down:
/// - `callback` is a function that is called synchronously to produce a future (`async` block).
/// - That resulting future (`R`) must itself return a "fulfill" function, or fail with a value stored in the [JsAsyncContext].
/// - The "fulfill" function (`F`) must produce a result in a synchronous JavaScript context, or fail with a JavaScript exception.
/// - That value (`V`) will be the final result of the JavaScript Promise.
/// - If there are any failures during the evaluation of the future or "fulfill" function, they will result in the rejection of the Promise.
/// - If there are any panics during the evaluation of the future or "fulfill" function, they will be translated to JavaScript Errors (per Neon conventions).
///
/// In practice, this is going to be the easiest way to produce a JavaScript Promise, especially for safely handling errors.
///
/// ```no_run
/// # use neon::prelude::*;
/// # use signal_neon_futures::*;
/// #
/// # struct TraitImpl;
/// # impl TraitImpl {
/// #   fn new(info: JsAsyncContextKey<JsObject>, context: JsAsyncContext) -> Self { Self }
/// # }
/// # async fn compute_result(t: TraitImpl) -> Result<String, JsAsyncContextKey<JsValue>> { Ok("abc".into()) }
/// #
/// fn js_compute_result(mut cx: FunctionContext) -> JsResult<JsObject> {
///     let js_info = cx.argument::<JsObject>(0)?;
///     promise(&mut cx, |cx, future_context| {
///         let saved_info = future_context.register_context_data(cx, js_info);
///         let trait_impl = TraitImpl::new(saved_info, future_context);
///         async move {
///             let result = compute_result(trait_impl).await?;
///             fulfill_promise(move |cx| Ok(cx.string(result)))
///         }
///     })
/// }
/// ```
pub fn promise<'a, V, F, R>(
    cx: &mut FunctionContext<'a>,
    callback: impl FnOnce(&mut FunctionContext<'a>, JsAsyncContext) -> R,
) -> JsResult<'a, JsObject>
where
    V: neon::types::Value,
    F: for<'b> FnOnce(&mut FunctionContext<'b>) -> JsResult<'b, V>,
    R: Future<Output = Result<F, JsAsyncContextKey<JsValue>>> + 'static,
{
    let mut promise = None;

    JsAsyncContext::run(cx, |cx, future_context| {
        let future = callback(cx, future_context.clone());

        let object = cx.empty_object();
        let save_promise_callbacks_fn = JsFunction::new(cx, save_promise_callbacks)?;
        let bind_fn: Handle<JsFunction> = save_promise_callbacks_fn
            .get(cx, "bind")?
            .downcast_or_throw(cx)?;
        let bound_save_promise_callbacks =
            bind_fn.call(cx, save_promise_callbacks_fn, vec![object])?;

        let promise_ctor: Handle<JsFunction> =
            cx.global().get(cx, "Promise")?.downcast_or_throw(cx)?;
        promise = Some(promise_ctor.construct(cx, vec![bound_save_promise_callbacks])?);

        let promise_object_key = future_context.register_context_data(cx, object);

        Ok(async move {
            // FIXME: This AssertUnwindSafe is actually a little sketchy!
            // Can JsFuture and JsAsyncContext be marked as UnwindSafe?
            let result = AssertUnwindSafe(future).catch_unwind().await;

            // Any JavaScript errors that happen here (as opposed to in the result callback)
            // are beyond our ability to recover.
            // Ignore them and let JsAsyncContext complain about the uncaught errors.
            let _ = future_context.with_context(|mut cx| -> NeonResult<()> {
                let resolved_result = match result {
                    Ok(Ok(resolve)) => {
                        // FIXME: This AssertUnwindSafe is likewise sketchy.
                        let resolve = AssertUnwindSafe(resolve);
                        // FIXME: This one's okay, though; the JS context can never be in an inconsistent state itself.
                        let mut cx = AssertUnwindSafe(&mut cx);
                        catch_unwind(move || cx.try_catch(|cx| resolve.0(cx)))
                    }
                    Ok(Err(saved_error)) => {
                        Ok(Err(future_context.get_context_data(cx, saved_error)))
                    }
                    Err(panic) => Err(panic),
                };
                let folded_result = resolved_result.unwrap_or_else(|panic| {
                    Err(cx
                        .error(format!("unexpected panic: {}", describe_any(&panic)))
                        .expect("can create an Error")
                        .upcast())
                });

                let (value, callback_name) = match folded_result {
                    Ok(value) => (value.upcast(), RESOLVE_SLOT),
                    Err(exception) => (exception, REJECT_SLOT),
                };

                let promise = future_context.get_context_data(cx, promise_object_key);
                let promise_callback: Handle<JsFunction> =
                    promise.get(cx, callback_name)?.downcast_or_throw(cx)?;
                promise_callback.call(cx, promise, vec![value])?;
                Ok(())
            });
        })
    })?;

    Ok(promise.unwrap())
}

/// Use this to return your "fulfill" function when using [promise()].
///
/// This works around a bug in the Rust compiler by providing extra type information.
/// It is equivalent to [Ok].
pub fn fulfill_promise<C, T, E>(callback: C) -> Result<C, E>
where
    T: Value,
    C: for<'a> FnOnce(&mut FunctionContext<'a>) -> JsResult<'a, T>,
{
    Ok(callback)
}
