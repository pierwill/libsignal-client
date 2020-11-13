//
// Copyright 2020 Signal Messenger, LLC.
// SPDX-License-Identifier: AGPL-3.0-only
//

use neon::prelude::*;
use std::cell::Cell;
use std::future::Future;
use std::marker::PhantomPinned;
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::pin::Pin;
use std::rc::{Rc, Weak};
use std::task::{Poll, Waker};

use crate::result::*;
use crate::*;

pub(crate) trait JsFutureCallback<T> =
    'static + for<'a> FnOnce(&mut FunctionContext<'a>, JsPromiseResult<'a>) -> T;

enum JsFutureState<T> {
    Waiting(JsAsyncContext, Box<dyn JsFutureCallback<T>>, Option<Waker>),
    Complete(std::thread::Result<T>),
    Consumed,
}

impl<T> JsFutureState<T> {
    fn new(context: JsAsyncContext, transform: impl JsFutureCallback<T>) -> Self {
        Self::Waiting(context, Box::new(transform), None)
    }

    fn waiting_on(self, waker: Waker) -> Self {
        if let Self::Waiting(context, transform, _) = self {
            Self::Waiting(context, transform, Some(waker))
        } else {
            panic!("already completed")
        }
    }
}

struct JsFutureShared<T> {
    state: Cell<JsFutureState<T>>,
    _pinned: PhantomPinned,
}

pub struct JsFuture<T> {
    shared: Pin<Rc<JsFutureShared<T>>>,
}

impl<T> JsFuture<T> {
    pub(crate) fn set_transform(&mut self, new_transform: impl JsFutureCallback<T>) {
        let mut state = self.shared.state.replace(JsFutureState::Consumed);
        match state {
            JsFutureState::Waiting(_, ref mut transform, _) => *transform = Box::new(new_transform),
            _ => panic!("already completed"),
        }
        self.shared.state.set(state);
    }

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
            JsFutureState::Waiting(ref async_context, _, None) => async_context.register_future(),
            JsFutureState::Waiting(_, _, _) => {}
        }
        self.shared.state.set(state.waiting_on(cx.waker().clone()));
        Poll::Pending
    }
}

fn resolve_promise<T, R: JsPromiseResultConstructor>(
    mut cx: FunctionContext,
) -> JsResult<JsUndefined> {
    let js_result = cx.argument(1)?;
    let opaque_ptr = cx.argument::<JsNumber>(0)?.value().to_bits() as *const ();
    let future = unsafe { Weak::from_raw(opaque_ptr as *const JsFutureShared<T>) };

    if let Some(future) = future.upgrade() {
        let state = future.state.replace(JsFutureState::Consumed);

        if let JsFutureState::Waiting(async_context, transform, waker) = state {
            async_context.resolve_future();
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
    pub fn new<'a>(
        cx: &mut FunctionContext<'a>,
        promise: Handle<'a, JsObject>,
        async_context: JsAsyncContext,
        transform: impl 'static + for<'b> FnOnce(&mut FunctionContext<'b>, JsPromiseResult<'b>) -> T,
    ) -> Self {
        let cell = Cell::new(JsFutureState::new(async_context, transform));
        let boxed = Rc::pin(JsFutureShared {
            state: cell,
            _pinned: PhantomPinned,
        });
        let boxed_ptr =
            unsafe { Rc::downgrade(&Pin::into_inner_unchecked(boxed.clone())).into_raw() };

        fn bound_resolve_promise<'a, T, R: JsPromiseResultConstructor>(
            cx: &mut FunctionContext<'a>,
            boxed_ptr: *const JsFutureShared<T>,
        ) -> Handle<'a, JsValue> {
            let resolve =
                JsFunction::new(cx, resolve_promise::<T, R>).expect("can create function");
            let bind = resolve
                .get(cx, "bind")
                .expect("bind() exists")
                .downcast::<JsFunction>()
                .expect("bind() is a function");
            let bind_args = vec![
                cx.undefined().upcast::<JsValue>(),
                cx.number(f64::from_bits(boxed_ptr as u64)).upcast(),
            ];
            bind.call(cx, resolve, bind_args).expect("can call bind()")
        }

        let bound_fulfill = bound_resolve_promise::<_, JsFulfilledResult>(cx, boxed_ptr);
        let bound_reject = bound_resolve_promise::<_, JsRejectedResult>(cx, boxed_ptr);

        let then = promise
            .get(cx, "then")
            .expect("then() exists")
            .downcast::<JsFunction>()
            .expect("then() is a function");
        then.call(cx, promise, vec![bound_fulfill, bound_reject])
            .expect("can call then()");

        Self { shared: boxed }
    }
}
