//
// Copyright 2020 Signal Messenger, LLC.
// SPDX-License-Identifier: AGPL-3.0-only
//

use neon::prelude::*;

pub type JsPromiseResult<'a> = Result<Handle<'a, JsValue>, Handle<'a, JsValue>>;

pub(crate) trait JsPromiseResultConstructor {
    fn make(value: Handle<JsValue>) -> JsPromiseResult;
}

pub(crate) struct JsFulfilledResult;

impl JsPromiseResultConstructor for JsFulfilledResult {
    fn make(value: Handle<JsValue>) -> JsPromiseResult {
        Ok(value)
    }
}

pub(crate) struct JsRejectedResult;

impl JsPromiseResultConstructor for JsRejectedResult {
    fn make(value: Handle<JsValue>) -> JsPromiseResult {
        Err(value)
    }
}
