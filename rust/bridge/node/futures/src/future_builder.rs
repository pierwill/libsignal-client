//
// Copyright 2020 Signal Messenger, LLC.
// SPDX-License-Identifier: AGPL-3.0-only
//

use neon::prelude::*;

use crate::*;

pub struct JsFutureBuilder<'a, T> {
    pub(crate) async_context: &'a JsAsyncContext,
    pub(crate) state: Result<JsFuture<T>, JsAsyncContextKey<JsValue>>,
}

impl<'a, T> JsFutureBuilder<'a, T> {
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
    pub fn then_try(
        self,
        transform: impl 'static
            + for<'b> FnOnce(&mut FunctionContext<'b>, JsPromiseResult<'b>) -> NeonResult<T>,
    ) -> JsFuture<Result<T, JsAsyncContextKey<JsValue>>> {
        let async_context = self.async_context.clone();
        self.then(move |cx, result| async_context.try_catch(cx, |cx| transform(cx, result)))
    }
}
