//
// Copyright 2020 Signal Messenger, LLC.
// SPDX-License-Identifier: AGPL-3.0-only
//

//! Allows `async` blocks to be used to wait on JavaScript futures using [Neon][].
//!
//! Neon provides a way to expose *synchronous* JavaScript functions from Rust.
//! This means that if Rust wants to wait for the result of a JavaScript promise,
//! it can at best return a callback to continue its work when the promise resolves.
//! This does not naturally compose with Rust's `async`, which works in terms of [Futures](trait@std::future::Future).
//!
//! This crate provides functionality for (1) wrapping JavaScript futures so they can be awaited on in Rust,
//! and (2) producing a JavaScript promise that wraps a Rust future. It does so by synchronously resuming execution
//! of the Rust future whenever an awaited JavaScript promise is resolved.
//!
//! This approach does have some caveats:
//! - The Neon JavaScript context is still lifetime-scoped, so data must be explicitly persisted across `await` points using [JsAsyncContext::register_context_data].
//! - Because execution is synchronous, no *other* kinds of blocking futures are supported. Any `await` must ultimately bottom out in a JsFuture.
//!
//! To get started, look at the [promise()] function and the [JsAsyncContext::await_promise] method.
//!
//! This crate is definitely pushing the limits of what Neon supports. Use outside of libsignal-client at your own risk.
//!
//! [Neon]: https://neon-bindings.com/

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
