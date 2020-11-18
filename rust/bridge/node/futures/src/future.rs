//
// Copyright 2020 Signal Messenger, LLC.
// SPDX-License-Identifier: AGPL-3.0-only
//

use neon::prelude::*;
use std::cell::Cell;
use std::future::Future;
use std::marker::PhantomPinned;
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe, RefUnwindSafe};
use std::pin::Pin;
use std::rc::{Rc, Weak};
use std::task::{Poll, Waker};

use crate::result::*;
use crate::*;

/// A transformation to convert a scoped JavaScript result (fulfillment or rejection) to an unscoped Rust result.
pub(crate) trait JsFutureCallback<T> =
    'static + for<'a> FnOnce(&mut FunctionContext<'a>, JsPromiseResult<'a>) -> T;

/// The possible states of a [JsFuture].
enum JsFutureState<T> {
    /// The future has been registered with the JavaScript runtime.
    Waiting {
        context: JsAsyncContext,
        transform: Box<dyn JsFutureCallback<T>>,
        waker: Option<Waker>,
    },
    /// The future has been resolved (and transformed).
    ///
    /// If there was a panic during that transform, it will be caught and resumed at the `await` point.
    Complete(std::thread::Result<T>),
    /// The result has been returned from `poll`.
    Consumed,
}

impl<T> JsFutureState<T> {
    fn new(context: JsAsyncContext, transform: impl JsFutureCallback<T>) -> Self {
        Self::Waiting {
            context,
            transform: Box::new(transform),
            waker: None,
        }
    }

    fn waiting_on(mut self, new_waker: Waker) -> Self {
        if let Self::Waiting { ref mut waker, .. } = self {
            *waker = Some(new_waker)
        } else {
            panic!("already completed")
        }
        self
    }
}

/// The shared state of a [JsFuture].
///
/// This is pinned so that we can pass a pointer to it to JavaScript.
/// In practice there will only be two references to one of these: a strong one from Rust,
/// and a weak one from JavaScript.
struct JsFutureShared<T> {
    state: Cell<JsFutureState<T>>,
    _pinned: PhantomPinned,
}

/// A shared JsFuture's Cell MUST never be left with an invalid state (unexpected Consumed) at a point where an uncaught panic could occur.
impl<T> RefUnwindSafe for JsFutureShared<T> {}

/// A future representing the result of a JavaScript promise.
///
/// JsFutures can be created with the [new](fn@JsFuture::new) method or with the convenience method [JsAsyncContext::await_promise].
/// Once resolved, a transformation callback is invoked in the current JavaScript context to produce a Rust value.
/// This is the result of `await`ing the future.
///
/// Panics in the transformation function will be propagated to the `await`ing context.
pub struct JsFuture<T> {
    shared: Pin<Rc<JsFutureShared<T>>>,
}

impl<T> JsFuture<T> {
    /// Replaces the current result callback with a new one.
    ///
    /// This is used by [JsFutureBuilder] to make the builder pattern nicer.
    pub(crate) fn with_transform(self, new_transform: impl JsFutureCallback<T>) -> Self {
        let mut state = self.shared.state.replace(JsFutureState::Consumed);
        match state {
            JsFutureState::Waiting {
                ref mut transform, ..
            } => *transform = Box::new(new_transform),
            _ => panic!("already completed"),
        }
        self.shared.state.set(state);
        self
    }

    /// Creates a completed future, as if already resolved.
    ///
    /// Used by [JsAsyncContext::await_promise] to treat thrown exceptions as rejections.
    pub(crate) fn ready(result: T) -> Self {
        Self {
            shared: Rc::pin(JsFutureShared {
                state: Cell::new(JsFutureState::Complete(Ok(result))),
                _pinned: PhantomPinned,
            }),
        }
    }
}

impl<T> Future for JsFuture<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut std::task::Context) -> Poll<Self::Output> {
        let state = self.shared.state.replace(JsFutureState::Consumed);
        match state {
            JsFutureState::Complete(Ok(result)) => return Poll::Ready(result),
            JsFutureState::Complete(Err(panic)) => resume_unwind(panic),
            JsFutureState::Consumed => panic!("already consumed"),
            JsFutureState::Waiting {
                context: ref async_context,
                transform: _,
                waker: None,
            } => async_context.register_future(),
            JsFutureState::Waiting { .. } => {}
        }
        self.shared.state.set(state.waiting_on(cx.waker().clone()));
        Poll::Pending
    }
}

/// Registered as the callback for the `resolve` and `reject` parameters of [`Promise.then`][then].
///
/// This callback assumes its first (bound) argument represents a `Weak<JsFutureShared<T>>`.
/// If the JsFutureShared is still alive, it is fulfilled and then execution of the top-level Future continues until it is blocked again.
///
/// [then]: https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/Promise/then
fn fulfill_promise<T, R: JsPromiseResultConstructor>(
    mut cx: FunctionContext,
) -> JsResult<JsUndefined> {
    let js_result = cx.argument(1)?;
    let opaque_ptr = cx.argument::<JsNumber>(0)?.value().to_bits() as *const ();
    let future = unsafe { Weak::from_raw(opaque_ptr as *const JsFutureShared<T>) };

    if let Some(future) = future.upgrade() {
        let state = future.state.replace(JsFutureState::Consumed);

        if let JsFutureState::Waiting {
            context: async_context,
            transform,
            waker,
        } = state
        {
            async_context.fulfill_future();
            async_context.run_with_context(&mut cx, |cx| {
                let result = catch_unwind(AssertUnwindSafe(|| transform(cx, R::make(js_result))));
                future.state.set(JsFutureState::Complete(result));
                if let Some(waker) = waker {
                    waker.wake()
                }
            });
        } else {
            panic!("already fulfilled");
        }
    }

    Ok(cx.undefined())
}

impl<T> JsFuture<T> {
    /// Creates a new JsFuture by calling the JavaScript method [`then`][then] on `promise`.
    ///
    /// When resolved, `transform` will be invoked in the new JavaScript context to produce the result of the Rust future.
    ///
    /// [then]: https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/Promise/then
    pub fn new<'a>(
        cx: &mut FunctionContext<'a>,
        promise: Handle<'a, JsObject>,
        async_context: JsAsyncContext,
        transform: impl 'static + for<'b> FnOnce(&mut FunctionContext<'b>, JsPromiseResult<'b>) -> T,
    ) -> NeonResult<Self> {
        let cell = Cell::new(JsFutureState::new(async_context, transform));
        let boxed = Rc::pin(JsFutureShared {
            state: cell,
            _pinned: PhantomPinned,
        });
        let boxed_ptr =
            unsafe { Rc::downgrade(&Pin::into_inner_unchecked(boxed.clone())).into_raw() };

        fn bound_fulfill_promise<'a, T, R: JsPromiseResultConstructor>(
            cx: &mut FunctionContext<'a>,
            boxed_ptr: *const JsFutureShared<T>,
        ) -> JsResult<'a, JsValue> {
            let fulfill = JsFunction::new(cx, fulfill_promise::<T, R>)?;
            let bind = fulfill
                .get(cx, "bind")?
                .downcast_or_throw::<JsFunction, _>(cx)?;
            let bind_args = vec![
                cx.undefined().upcast::<JsValue>(),
                cx.number(f64::from_bits(boxed_ptr as u64)).upcast(),
            ];
            bind.call(cx, fulfill, bind_args)
        }

        let bound_resolve = bound_fulfill_promise::<_, JsResolvedResult>(cx, boxed_ptr)?;
        let bound_reject = bound_fulfill_promise::<_, JsRejectedResult>(cx, boxed_ptr)?;

        let then = promise
            .get(cx, "then")?
            .downcast_or_throw::<JsFunction, _>(cx)?;
        then.call(cx, promise, vec![bound_resolve, bound_reject])?;

        Ok(Self { shared: boxed })
    }
}
