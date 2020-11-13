//
// Copyright 2020 Signal Messenger, LLC.
// SPDX-License-Identifier: AGPL-3.0-only
//

use neon::prelude::*;
use signal_neon_futures::*;

#[allow(unreachable_code, unused_variables)]
pub fn panic_pre_await(mut cx: FunctionContext) -> JsResult<JsObject> {
    let promise = cx.argument::<JsObject>(0)?;

    signal_neon_futures::promise(&mut cx, |cx, future_context| {
        let future = JsFuture::new(cx, promise, future_context.clone(), move |cx, result| {
            future_context.try_catch(cx, |cx| {
                let value = result.or_else(|e| cx.throw(e))?;
                Ok(value.downcast_or_throw::<JsNumber, _>(cx)?.value())
            })
        });
        async move {
            panic!("check for this");
            future.await?;
            fulfill_promise(move |cx| Ok(cx.undefined()))
        }
    })
}

#[allow(unreachable_code)]
pub fn panic_during_callback(mut cx: FunctionContext) -> JsResult<JsObject> {
    let promise = cx.argument::<JsObject>(0)?;

    signal_neon_futures::promise(&mut cx, |cx, future_context| {
        let future = JsFuture::new(cx, promise, future_context, |_cx, _result| {
            panic!("check for this")
        });
        async move {
            future.await;
            fulfill_promise(move |cx| Ok(cx.undefined()))
        }
    })
}

#[allow(unreachable_code)]
pub fn panic_post_await(mut cx: FunctionContext) -> JsResult<JsObject> {
    let promise = cx.argument::<JsObject>(0)?;

    signal_neon_futures::promise(&mut cx, |cx, future_context| {
        let future = JsFuture::new(cx, promise, future_context.clone(), move |cx, result| {
            future_context.try_catch(cx, |cx| {
                let value = result.or_else(|e| cx.throw(e))?;
                Ok(value.downcast_or_throw::<JsNumber, _>(cx)?.value())
            })
        });
        async move {
            future.await?;
            panic!("check for this");
            fulfill_promise(move |cx| Ok(cx.undefined()))
        }
    })
}

#[allow(unreachable_code, unused_variables)]
pub fn panic_during_fulfill(mut cx: FunctionContext) -> JsResult<JsObject> {
    let promise = cx.argument::<JsObject>(0)?;

    signal_neon_futures::promise(&mut cx, |cx, future_context| {
        let future = JsFuture::new(cx, promise, future_context.clone(), move |cx, result| {
            future_context.try_catch(cx, |cx| {
                let value = result.or_else(|e| cx.throw(e))?;
                Ok(value.downcast_or_throw::<JsNumber, _>(cx)?.value())
            })
        });
        async move {
            future.await?;
            fulfill_promise(move |cx| {
                panic!("check for this");
                Ok(cx.undefined())
            })
        }
    })
}

pub fn throw_pre_await(mut cx: FunctionContext) -> JsResult<JsObject> {
    let promise = cx.argument::<JsObject>(0)?;

    signal_neon_futures::promise(&mut cx, |cx, future_context| {
        let future_context_for_callback = future_context.clone();
        let future = JsFuture::new(cx, promise, future_context.clone(), move |cx, result| {
            future_context_for_callback.try_catch(cx, |cx| {
                let value = result.or_else(|e| cx.throw(e))?;
                Ok(value.downcast_or_throw::<JsNumber, _>(cx)?.value())
            })
        });
        async move {
            future_context
                .try_with_context(|cx| -> NeonResult<()> { cx.throw_error("check for this") })?;
            future.await?;
            fulfill_promise(move |cx| Ok(cx.undefined()))
        }
    })
}

pub fn throw_during_callback(mut cx: FunctionContext) -> JsResult<JsObject> {
    let promise = cx.argument::<JsObject>(0)?;

    signal_neon_futures::promise(&mut cx, |cx, future_context| {
        let future = JsFuture::new(cx, promise, future_context.clone(), move |cx, _result| {
            future_context.try_catch(cx, |cx| {
                cx.throw_error("check for this")?;
                Ok(())
            })
        });
        async move {
            future.await?;
            fulfill_promise(move |cx| Ok(cx.undefined()))
        }
    })
}

pub fn throw_post_await(mut cx: FunctionContext) -> JsResult<JsObject> {
    let promise = cx.argument::<JsObject>(0)?;

    signal_neon_futures::promise(&mut cx, |cx, future_context| {
        let future_context_for_callback = future_context.clone();
        let future = JsFuture::new(cx, promise, future_context.clone(), move |cx, result| {
            future_context_for_callback.try_catch(cx, |cx| {
                let value = result.or_else(|e| cx.throw(e))?;
                Ok(value.downcast_or_throw::<JsNumber, _>(cx)?.value())
            })
        });
        async move {
            future.await?;
            future_context
                .try_with_context(|cx| -> NeonResult<()> { cx.throw_error("check for this") })?;
            fulfill_promise(move |cx| Ok(cx.undefined()))
        }
    })
}

pub fn throw_during_fulfill(mut cx: FunctionContext) -> JsResult<JsObject> {
    let promise = cx.argument::<JsObject>(0)?;

    signal_neon_futures::promise(&mut cx, |cx, future_context| {
        let future = JsFuture::new(cx, promise, future_context.clone(), move |cx, result| {
            future_context.try_catch(cx, |cx| {
                let value = result.or_else(|e| cx.throw(e))?;
                Ok(value.downcast_or_throw::<JsNumber, _>(cx)?.value())
            })
        });
        async move {
            future.await?;
            fulfill_promise(move |cx| -> JsResult<JsUndefined> { cx.throw_error("check for this") })
        }
    })
}
