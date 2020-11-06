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
    let promise_key = future_context.register_context_data(&mut cx, promise)?;
    let completion_callback_key =
        future_context.register_context_data(&mut cx, completion_callback)?;

    future_context.clone().run(&mut cx, async move {
        let future = future_context
            .await_promise(|cx| {
                let promise = future_context
                    .get_context_data(cx, promise_key)
                    .expect("exists");
                promise
            })
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
                .expect("exists")
                .call(cx, undefined, vec![new_value])
                .expect("call succeeds");
        });
    });

    Ok(cx.undefined())
}

register_module!(mut cx, {
    cx.export_function("incrementAsync", increment_async)
});
