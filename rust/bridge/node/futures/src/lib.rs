//
// Copyright 2020 Signal Messenger, LLC.
// SPDX-License-Identifier: AGPL-3.0-only
//

#![feature(cell_leak)]
#![feature(trait_alias)]
#![warn(missing_docs)]

mod context;
pub use context::{JsAsyncContext, JsAsyncContextKey};

mod future;
pub use future::JsFuture;

mod future_builder;
pub use future_builder::JsFutureBuilder;

mod result;
pub use result::JsPromiseResult;

mod promise;
pub use promise::{fulfill_promise, promise};

mod util;
