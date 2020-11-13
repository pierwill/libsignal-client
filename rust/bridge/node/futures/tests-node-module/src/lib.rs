//
// Copyright 2020 Signal Messenger, LLC.
// SPDX-License-Identifier: AGPL-3.0-only
//

use neon::prelude::*;
use signal_neon_futures::*;

fn increment_async(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let promise = cx.argument::<JsObject>(0)?;
    let completion_callback = cx.argument::<JsFunction>(1)?;

    let future_context = JsAsyncContext::new();
    let promise_key = future_context.register_context_data(&mut cx, promise);
    let completion_callback_key =
        future_context.register_context_data(&mut cx, completion_callback);

    future_context.clone().run(&mut cx, async move {
        let future = future_context
            .await_promise(|cx| future_context.get_context_data(cx, promise_key))
            .then(|cx, result| match result {
                Ok(value) => Ok(value.downcast::<JsNumber>().expect("is number").value()),
                Err(err) => Err(err.to_string(cx).unwrap().value()),
            });
        let value_or_error = future
            .await
            .unwrap_or_else(|_| panic!("cannot fail on the JS side"));
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

fn panic_on_resolve(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let promise = cx.argument::<JsObject>(0)?;

    let future_context = JsAsyncContext::new();
    let promise_key = future_context.register_context_data(&mut cx, promise);

    future_context.clone().run(&mut cx, async move {
        let future = future_context
            .await_promise(|cx| future_context.get_context_data(cx, promise_key))
            .then(|_cx, _result| panic!("hello"));
        future.await;
    });

    Ok(cx.undefined())
}

fn increment_promise(mut cx: FunctionContext) -> JsResult<JsObject> {
    let promise = cx.argument::<JsObject>(0)?;

    signal_neon_futures::promise(&mut cx, |cx, future_context| {
        let future = JsFuture::new(cx, promise, future_context, |cx, result| {
            let value = result.or_else(|e| cx.throw(e))?;
            Ok(value.downcast_or_throw::<JsNumber, _>(cx)?.value())
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
    cx.export_function("panicOnResolve", panic_on_resolve)?;
    Ok(())
});
