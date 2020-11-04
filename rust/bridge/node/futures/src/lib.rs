//
// Copyright 2020 Signal Messenger, LLC.
// SPDX-License-Identifier: AGPL-3.0-only
//

#![feature(cell_leak)]

mod context;
pub use context::{JsAsyncContext, JsAsyncContextKey};

mod future;
pub use future::JsFuture;

mod result;
pub use result::JsFutureResult;
