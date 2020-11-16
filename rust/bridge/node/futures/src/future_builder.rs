//
// Copyright 2020 Signal Messenger, LLC.
// SPDX-License-Identifier: AGPL-3.0-only
//

use neon::prelude::*;

use crate::*;

/// Builds a [JsFuture] in a particular [JsAsyncContext].
///
/// [JsFutureBuilder] provides conveniences for handling the result of a JavaScript promise.
pub struct JsFutureBuilder<'a, T> {
    pub(crate) async_context: &'a JsAsyncContext,
    pub(crate) state: Result<JsFuture<T>, JsAsyncContextKey<JsValue>>,
}

impl<'a, T> JsFutureBuilder<'a, T> {
    /// Produces a future using the given result handler.
    ///
    /// Note that if there was a JavaScript exception during the creation of this JsFutureBuilder,
    /// `transform` will be called **immediately** to produce a result,
    /// treating the exception as a promise rejection.
    pub fn then(
        self,
        transform: impl 'static + for<'b> FnOnce(&mut FunctionContext<'b>, JsPromiseResult<'b>) -> T,
    ) -> JsFuture<T> {
        match self.state {
            Ok(mut future) => {
                future.set_transform(transform);
                future
            }
            Err(key) => self.async_context.with_context(|cx| {
                let exception = self.async_context.get_context_data(cx, key);
                let result = transform(cx, Err(exception));
                JsFuture::ready(result)
            }),
        }
    }
}

impl<'a, T> JsFutureBuilder<'a, Result<T, JsAsyncContextKey<JsValue>>> {
    /// Produces a future that stores failure in the JsAsyncContext.
    ///
    /// This is a convenience to allow the result handler to throw JavaScript exceptions using NeonResult.
    /// Note that this does *not* automatically treat incoming rejections as failures; if that is desired,
    /// it can be accomplished using `result.or_else(|e| cx.throw(e))?;` in the body of `transform`.
    pub fn then_try(
        self,
        transform: impl 'static
            + for<'b> FnOnce(&mut FunctionContext<'b>, JsPromiseResult<'b>) -> NeonResult<T>,
    ) -> JsFuture<Result<T, JsAsyncContextKey<JsValue>>> {
        let async_context = self.async_context.clone();
        self.then(move |cx, result| async_context.try_catch(cx, |cx| transform(cx, result)))
    }
}
