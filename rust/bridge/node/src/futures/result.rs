//
// Copyright (C) 2020 Signal Messenger, LLC.
// All rights reserved.
//
// SPDX-License-Identifier: GPL-3.0-only
//

use neon::prelude::*;

pub type JsFutureResult<'a> = Result<Handle<'a, JsValue>, Handle<'a, JsValue>>;

pub(in crate::futures) trait JsFutureResultConstructor {
    fn make(value: Handle<JsValue>) -> JsFutureResult;
}

pub(in crate::futures) struct JsFulfilledResult;

impl JsFutureResultConstructor for JsFulfilledResult {
    fn make(value: Handle<JsValue>) -> JsFutureResult {
        Ok(value)
    }
}

pub(in crate::futures) struct JsRejectedResult;

impl JsFutureResultConstructor for JsRejectedResult {
    fn make(value: Handle<JsValue>) -> JsFutureResult {
        Err(value)
    }
}
