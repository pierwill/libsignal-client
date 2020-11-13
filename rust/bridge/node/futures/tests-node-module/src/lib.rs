//
// Copyright 2020 Signal Messenger, LLC.
// SPDX-License-Identifier: AGPL-3.0-only
//

use neon::prelude::*;
use signal_neon_futures::*;

mod panics_and_throws;
use panics_and_throws::*;

mod store_like;
use store_like::*;

fn increment_async(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let promise = cx.argument::<JsObject>(0)?;
    let completion_callback = cx.argument::<JsFunction>(1)?;

    let future_context = JsAsyncContext::new();
    let promise_key = future_context.register_context_data(&mut cx, promise);
    let completion_callback_key =
        future_context.register_context_data(&mut cx, completion_callback);

    future_context.clone().run(&mut cx, async move {
        let future = future_context
            .await_promise(|cx| Ok(future_context.get_context_data(cx, promise_key)))
            .then(|cx, result| match result {
                Ok(value) => Ok(value.downcast::<JsNumber>().expect("is number").value()),
                Err(err) => Err(err.to_string(cx).unwrap().value()),
            });
        let value_or_error = future.await;
        future_context.with_context(|cx| {
            let new_value = match value_or_error {
                Ok(value) => cx.number(value + 1.0).upcast::<JsValue>(),
                Err(ref message) => cx.string(format!("error: {}", message)).upcast::<JsValue>(),
            };
            let undefined = cx.undefined();
            future_context
                .get_context_data(cx, completion_callback_key)
                .call(cx, undefined, vec![new_value])
                .expect("call succeeds");
        });
    });

    Ok(cx.undefined())
}

fn increment_promise(mut cx: FunctionContext) -> JsResult<JsObject> {
    let promise = cx.argument::<JsObject>(0)?;

    signal_neon_futures::promise(&mut cx, |cx, future_context| {
        let future = JsFuture::new(cx, promise, future_context.clone(), move |cx, result| {
            future_context.try_catch(cx, |cx| {
                let value = result.or_else(|e| cx.throw(e))?;
                Ok(value.downcast_or_throw::<JsNumber, _>(cx)?.value())
            })
        });
        async move {
            let value = future.await?;
            fulfill_promise(move |cx| Ok(cx.number(value + 1.0)))
        }
    })
}

register_module!(mut cx, {
    cx.export_function("incrementAsync", increment_async)?;
    cx.export_function("incrementPromise", increment_promise)?;

    cx.export_function("doubleNameFromStore", double_name_from_store)?;

    cx.export_function("panicPreAwait", panic_pre_await)?;
    cx.export_function("panicDuringCallback", panic_during_callback)?;
    cx.export_function("panicPostAwait", panic_post_await)?;
    cx.export_function("panicDuringFulfill", panic_during_fulfill)?;

    cx.export_function("throwPreAwait", throw_pre_await)?;
    cx.export_function("throwDuringCallback", throw_during_callback)?;
    cx.export_function("throwPostAwait", throw_post_await)?;
    cx.export_function("throwDuringFulfill", throw_during_fulfill)?;

    Ok(())
});
