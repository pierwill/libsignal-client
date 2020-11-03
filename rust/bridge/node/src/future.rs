//
// Copyright (C) 2020 Signal Messenger, LLC.
// All rights reserved.
//
// SPDX-License-Identifier: GPL-3.0-only
//

use futures::executor::LocalPool;
use futures::task::LocalSpawnExt;
use neon::prelude::*;
use std::cell::{Cell, RefCell};
use std::future::Future;
use std::marker::{PhantomData, PhantomPinned};
use std::panic;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Poll, Waker};

pub type JsFutureResult<'a> = Result<Handle<'a, JsValue>, Handle<'a, JsValue>>;

trait JsFutureResultConstructor {
    fn make(value: Handle<JsValue>) -> JsFutureResult;
}

struct JsFulfilledResult;

impl JsFutureResultConstructor for JsFulfilledResult {
    fn make(value: Handle<JsValue>) -> JsFutureResult {
        Ok(value)
    }
}

struct JsRejectedResult;

impl JsFutureResultConstructor for JsRejectedResult {
    fn make(value: Handle<JsValue>) -> JsFutureResult {
        Err(value)
    }
}

pub type JsFutureCallback<T> = for<'a> fn(&mut FunctionContext<'a>, JsFutureResult<'a>) -> T;

enum JsFutureState<T> {
    Waiting(JsAsyncContext, JsFutureCallback<T>, Option<Waker>),
    Complete(T),
    Consumed,
}

impl<T> JsFutureState<T> {
    fn new(context: JsAsyncContext, transform: JsFutureCallback<T>) -> Self {
        Self::Waiting(context, transform, None)
    }

    fn waiting_on(self, waker: Waker) -> Self {
        if let Self::Waiting(context, transform, _) = self {
            Self::Waiting(context, transform, Some(waker))
        } else {
            panic!("already completed")
        }
    }
}

pub struct JsFuture<T> {
    state: Cell<JsFutureState<T>>,
    _pinned: PhantomPinned,
}

impl<T> Future for JsFuture<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut std::task::Context) -> Poll<Self::Output> {
        let state = self.state.replace(JsFutureState::Consumed);
        match state {
            JsFutureState::Complete(result) => return Poll::Ready(result),
            JsFutureState::Consumed => panic!("already consumed"),
            JsFutureState::Waiting(ref async_context, _, None) => async_context.register_future(),
            JsFutureState::Waiting(_, _, _) => {}
        }
        self.state.set(state.waiting_on(cx.waker().clone()));
        Poll::Pending
    }
}

fn resolve_promise<T, R: JsFutureResultConstructor>(
    mut cx: FunctionContext,
) -> JsResult<JsUndefined> {
    let js_result = cx.argument(1)?;
    let opaque_ptr = cx.argument::<JsNumber>(0)?.value() as u64 as *const ();
    let future = unsafe { (opaque_ptr as *const JsFuture<T>).as_ref().unwrap() };
    let state = future.state.replace(JsFutureState::Consumed);

    if let JsFutureState::Waiting(async_context, transform, waker) = state {
        let result = transform(&mut cx, R::make(js_result));
        future.state.set(JsFutureState::Complete(result));
        async_context.resolve_future();
        async_context.run_with_context(&mut cx, || {
            if let Some(waker) = waker {
                waker.wake()
            }
        });
    } else {
        panic!("already fulfilled");
    }

    Ok(cx.undefined())
}

impl<T> JsFuture<T> {
    pub fn new<'a>(
        cx: &mut FunctionContext<'a>,
        promise: Handle<'a, JsObject>,
        async_context: JsAsyncContext,
        transform: JsFutureCallback<T>,
    ) -> Pin<Box<Self>> {
        let cell = Cell::new(JsFutureState::new(async_context, transform));
        let boxed = Box::pin(Self {
            state: cell,
            _pinned: PhantomPinned,
        });
        let boxed_ptr = &(*boxed) as *const Self;

        fn bound_resolve_promise<'a, T, R: JsFutureResultConstructor>(
            cx: &mut FunctionContext<'a>,
            boxed_ptr: *const T,
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
                cx.number(boxed_ptr as u64 as f64).upcast(),
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

        boxed
    }
}

pub struct JsFutureBuilder<T> {
    future: Pin<Box<JsFuture<T>>>,
}

impl<T> JsFutureBuilder<T> {
    pub fn then(self, transform: JsFutureCallback<T>) -> Pin<Box<JsFuture<T>>> {
        let state = self.future.state.replace(JsFutureState::Consumed);
        if let JsFutureState::Waiting(async_context, _, None) = state {
            self.future
                .state
                .set(JsFutureState::new(async_context, transform));
            self.future
        } else {
            panic!("then() must be called immediately after await_promise()")
        }
    }
}

trait CurrentContext {
    fn with_context(&self, callback: &mut dyn for<'a> FnMut(&mut FunctionContext<'a>));
}

struct CurrentContextImpl<'a, 'b> {
    context: &'b RefCell<FunctionContext<'a>>,
}

impl<'a, 'b> CurrentContext for CurrentContextImpl<'a, 'b> {
    fn with_context(&self, callback: &mut dyn for<'c> FnMut(&mut FunctionContext<'c>)) {
        callback(&mut self.context.borrow_mut())
    }
}

struct JsAsyncContextImpl {
    // This 'static is a lie that allows the JavaScript context to be accessed from arbitrary use sites.
    // Be very very careful that you do not persist this reference.
    // Also do not share the 'static outside the implementation of JsAsyncContextImpl.
    very_unsafe_current_context: Option<&'static dyn CurrentContext>,

    num_pending_js_futures: i32,
    complete: bool,
    pool: Option<LocalPool>,
    global_key: String,
    num_globals: u32,
    _pinned: PhantomPinned,
}

impl Drop for JsAsyncContextImpl {
    fn drop(&mut self) {
        let current_context = self.very_unsafe_current_context.expect(
            "must clean up JsAsyncContext during the fulfillment of the last JS callback invoked",
        );
        current_context.with_context(&mut |cx| {
            let undef = cx.undefined();
            cx.global()
                .set(cx, self.global_key.as_str(), undef)
                .expect("no one else cleared this");
        });
    }
}

// Based on https://crates.io/crates/take_mut.
fn as_ref_cell<T, F>(mut_ref: &mut T, closure: F)
where
    F: FnOnce(&RefCell<T>),
{
    use std::ptr;

    unsafe {
        let mut cell = RefCell::new(ptr::read(mut_ref));
        let result = panic::catch_unwind(panic::AssertUnwindSafe(|| closure(&cell)));
        cell.undo_leak();
        ptr::write(mut_ref, cell.into_inner());
        if let Err(err) = result {
            panic::resume_unwind(err);
        }
    }
}

#[derive(Clone, Copy)]
pub struct JsAsyncContextKey<T: neon::types::Value> {
    raw_key: u32,
    // https://doc.rust-lang.org/std/marker/struct.PhantomData.html#ownership-and-the-drop-check
    _type: PhantomData<*const T>,
}

#[derive(Clone)]
pub struct JsAsyncContext {
    shared_state: Pin<Rc<RefCell<JsAsyncContextImpl>>>,
}

impl JsAsyncContext {
    fn run_with_context(self, cx: &mut FunctionContext, action: impl FnOnce()) {
        // While running, we use a RefCell to dynamically check access to the JS context.
        // But a RefCell has to own its data. as_ref_cell allows us to take the context out of its current reference and put it back later, like a more advanced version of std::mem::replace.
        as_ref_cell(cx, |cx| {
            let c: &dyn CurrentContext = &CurrentContextImpl { context: cx };
            // Lie about the lifetime of `c` so that it can be accessed from arbitrary call sites.
            // "This is advanced, very unsafe Rust!" - std::mem::transmute docs on lifetime extension.
            #[allow(clippy::transmute_ptr_to_ptr)]
            let opaque_c = unsafe {
                std::mem::transmute::<&dyn CurrentContext, &'static dyn CurrentContext>(c)
            };

            let prev_context = self
                .shared_state
                .borrow_mut()
                .very_unsafe_current_context
                .replace(opaque_c);

            action();

            let maybe_pool = self.shared_state.borrow_mut().pool.take();
            if let Some(mut pool) = maybe_pool {
                pool.run_until_stalled();
                let mut shared_state = self.shared_state.borrow_mut();
                assert!(
                    shared_state.complete || shared_state.num_pending_js_futures > 0,
                    "only supports blocking on JavaScript futures"
                );
                shared_state.pool = Some(pool);
            } else {
                // We're in a recursive call and the pool will continue to be processed when we return.
            }

            // If this is the last reference, destroy it while the JavaScript context is still available.
            match Rc::try_unwrap(unsafe { Pin::into_inner_unchecked(self.shared_state) }) {
                Ok(state) => std::mem::drop(state),
                Err(shared_state) => {
                    shared_state.borrow_mut().very_unsafe_current_context = prev_context
                }
            }
        });
    }

    fn register_future(&self) {
        self.shared_state.borrow_mut().num_pending_js_futures += 1
    }

    fn resolve_future(&self) {
        self.shared_state.borrow_mut().num_pending_js_futures -= 1
    }

    pub fn new() -> Self {
        let result = Self {
            shared_state: Rc::pin(RefCell::new(JsAsyncContextImpl {
                very_unsafe_current_context: None,
                num_pending_js_futures: 0,
                complete: false,
                pool: Some(LocalPool::new()),
                global_key: String::new(), // replaced below based on the address this object gets pinned to
                num_globals: 0,
                _pinned: PhantomPinned,
            })),
        };
        result.shared_state.borrow_mut().global_key = format!(
            "__libsignal-client::JsAsyncContext::{:x}",
            result.shared_state.as_ptr() as usize
        );
        result
    }

    pub fn with_context<R>(
        &self,
        mut callback: impl for<'a> FnMut(&mut FunctionContext<'a>) -> R,
    ) -> R {
        let context_holder = self
            .shared_state
            .borrow()
            .very_unsafe_current_context
            .expect("cannot use the JS context outside of a JS call");
        let mut result = None;
        context_holder.with_context(&mut |cx| result = Some(callback(cx)));
        result.unwrap() // The callback is always called; we just can't prove it to the compiler.
    }

    pub fn run(self, cx: &mut FunctionContext, future: impl Future<Output = ()> + 'static) {
        let spawner = self
            .shared_state
            .borrow()
            .pool
            .as_ref()
            .expect("should only be called at the top level of an operation")
            .spawner();
        let self_for_future = self.clone();
        spawner
            .spawn_local(async move {
                future.await;
                self_for_future.shared_state.borrow_mut().complete = true;
            })
            .expect("can spawn at the top level of an operation");
        self.run_with_context(cx, || {});
    }

    fn context_data_object<'a>(
        &self,
        cx: &mut FunctionContext<'a>,
        create_if_needed: bool,
    ) -> Handle<'a, JsObject> {
        let global_key = &self.shared_state.borrow().global_key;

        let context_storage = cx
            .global()
            .get(cx, global_key.as_ref())
            .expect("can always get properties on the global object");
        if create_if_needed && context_storage.is_a::<JsUndefined>() {
            let new_storage = cx.empty_object();
            cx.global()
                .set(cx, global_key.as_ref(), new_storage)
                .expect("can always set properties on the global object");
            new_storage
        } else {
            context_storage
                .downcast()
                .expect("context data accessed improperly somehow")
        }
    }

    pub fn await_promise<T>(
        &self,
        mut promise_callback: impl for<'a> FnMut(&mut FunctionContext<'a>) -> Handle<'a, JsObject>,
    ) -> JsFutureBuilder<T> {
        let future = self.with_context(|cx| {
            let promise = promise_callback(cx);
            JsFuture::new(cx, promise, self.clone(), |_cx, _handle| {
                panic!("no transform set yet") // FIXME
            })
        });
        JsFutureBuilder { future }
    }

    pub fn register_context_data<'a, T: neon::types::Value>(
        &self,
        cx: &mut FunctionContext<'a>,
        value: Handle<'a, T>,
    ) -> NeonResult<JsAsyncContextKey<T>> {
        let raw_key = self.shared_state.borrow().num_globals;
        self.shared_state.borrow_mut().num_globals += 1;
        self.context_data_object(cx, true).set(cx, raw_key, value)?;
        Ok(JsAsyncContextKey {
            raw_key,
            _type: PhantomData,
        })
    }

    pub fn get_context_data<'a, T: neon::types::Value>(
        &self,
        cx: &mut FunctionContext<'a>,
        key: JsAsyncContextKey<T>,
    ) -> JsResult<'a, T> {
        self.context_data_object(cx, false)
            .get(cx, key.raw_key)?
            .downcast_or_throw(cx)
    }
}
