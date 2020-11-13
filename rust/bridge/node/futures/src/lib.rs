//
// Copyright 2020 Signal Messenger, LLC.
// SPDX-License-Identifier: AGPL-3.0-only
//

#![feature(cell_leak)]
#![feature(trait_alias)]

mod context;
pub use context::{JsAsyncContext, JsAsyncContextKey};

mod future;
pub use future::JsFuture;

mod future_builder;

mod result;
pub use result::JsPromiseResult;

mod promise;
pub use promise::{fulfill_promise, promise};

mod util;
