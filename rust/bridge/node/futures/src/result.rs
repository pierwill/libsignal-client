//
// Copyright 2020 Signal Messenger, LLC.
// SPDX-License-Identifier: AGPL-3.0-only
//

use neon::prelude::*;

pub type JsFutureResult<'a> = Result<Handle<'a, JsValue>, Handle<'a, JsValue>>;

pub(crate) trait JsFutureResultConstructor {
    fn make(value: Handle<JsValue>) -> JsFutureResult;
}

pub(crate) struct JsFulfilledResult;

impl JsFutureResultConstructor for JsFulfilledResult {
    fn make(value: Handle<JsValue>) -> JsFutureResult {
        Ok(value)
    }
}

pub(crate) struct JsRejectedResult;

impl JsFutureResultConstructor for JsRejectedResult {
    fn make(value: Handle<JsValue>) -> JsFutureResult {
        Err(value)
    }
}
