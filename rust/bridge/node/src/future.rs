//
// Copyright (C) 2020 Signal Messenger, LLC.
// All rights reserved.
//
// SPDX-License-Identifier: GPL-3.0-only
//

use neon::prelude::*;
use std::future::Future;
use std::task::{Poll,Waker};
use std::pin::Pin;
use std::cell::{Cell,RefCell};
use std::rc::Rc;
use std::marker::{PhantomData,PhantomPinned};
use std::panic;
use futures::executor::LocalPool;
use futures::task::LocalSpawnExt;

type JsFulfillmentCallback<T> = for<'a> fn(&mut FunctionContext<'a>, Handle<'a, JsValue>) -> T;
type OpaqueJsFulfillmentCallback = for<'a> fn(&mut FunctionContext<'a>, Handle<'a, JsValue>, *const());

enum JsFutureState<T> {
    Started(JsAsyncContext),
    Waiting(JsAsyncContext, Waker),
    Complete(T),
    Consumed,
}

impl<T> JsFutureState<T> {
    fn into_async_context(self) -> JsAsyncContext {
        match self {
            JsFutureState::Started(context) | JsFutureState::Waiting(context, _) => context,
            JsFutureState::Complete(_) | JsFutureState::Consumed => panic!("already completed"),
        }
    }
}

struct JsFutureInfo<T> {
    transform: JsFulfillmentCallback<T>,
    state: JsFutureState<T>,
}

impl<T> JsFutureInfo<T> {
    fn new(transform: JsFulfillmentCallback<T>, context: JsAsyncContext) -> Self {
        Self {
            transform,
            state: JsFutureState::Started(context),
        }
    }

    fn waiting_on(self, waker: Waker) -> Self {
        Self {
            transform: self.transform,
            state: JsFutureState::Waiting(self.state.into_async_context(), waker),
        }
    }

    fn complete(value: T) -> Self {
        Self {
            transform: |_cx, _handle| panic!("already completed"),
            state: JsFutureState::Complete(value),
        }
    }

    fn consumed() -> Self {
        Self {
            transform: |_cx, _handle| panic!("already consumed"),
            state: JsFutureState::Consumed,
        }
    }
}


pub struct JsFuture<T> {
    info: Cell<JsFutureInfo<T>>,
    _pinned: PhantomPinned,
}

impl <T> Future for JsFuture<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut std::task::Context) -> Poll<Self::Output> {
        let info = self.info.replace(JsFutureInfo::consumed());
        match info.state {
            JsFutureState::Complete(result) => return Poll::Ready(result),
            JsFutureState::Consumed => panic!("already consumed"),
            JsFutureState::Started(ref async_context) => async_context.register_future(),
            JsFutureState::Waiting(_, _) => {}
        }
        self.info.set(info.waiting_on(cx.waker().clone()));
        Poll::Pending
    }
}

fn fulfill(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let result = cx.argument(2)?;
    let cell_ptr = cx.argument::<JsNumber>(1)?.value() as u64 as *const ();
    let save_result_ptr = cx.argument::<JsNumber>(0)?.value() as u64 as *const ();
    let save_result = unsafe { std::mem::transmute::<_, OpaqueJsFulfillmentCallback>(save_result_ptr) };
    save_result(&mut cx, result, cell_ptr);
    Ok(cx.undefined())
}

fn get_save_result_fn<T>() -> OpaqueJsFulfillmentCallback {
    return |cx, js_result, opaque_ptr| {
        let future = unsafe { (opaque_ptr as *const JsFuture<T>).as_ref().unwrap() };
        let info = future.info.replace(JsFutureInfo::consumed());
        let result = (info.transform)(cx, js_result);
        future.info.set(JsFutureInfo::complete(result));

        match info.state {
            JsFutureState::Started(async_context) => {
                async_context.resolve_future();
                async_context.run_with_context(cx, || {});
            }
            JsFutureState::Waiting(async_context, waker) => {
                async_context.resolve_future();
                async_context.run_with_context(cx, || waker.wake());
            }
            _ => {
                panic!("already fulfilled")
            }
        }
    }
}

impl<T> JsFuture<T> {
    pub fn new<'a, C>(cx: &mut C, promise: Handle<'a, JsObject>, async_context: JsAsyncContext, transform: JsFulfillmentCallback<T>) -> Pin<Box<Self>> where C: Context<'a> {
        let cell = Cell::new(JsFutureInfo::new(transform, async_context));
        let boxed = Box::pin(Self { info: cell, _pinned: PhantomPinned });
        let boxed_ptr = &(*boxed) as *const Self;
        let save_result_ptr = unsafe { std::mem::transmute::<_, *const ()>(get_save_result_fn::<T>()) };

        let fulfill = JsFunction::new(cx, fulfill).expect("can create function");
        let bind = fulfill.get(cx, "bind").expect("bind() exists").downcast::<JsFunction>().expect("bind() is a function");
        let bind_args = vec![cx.undefined().upcast::<JsValue>(), cx.number(save_result_ptr as u64 as f64).upcast(), cx.number(boxed_ptr as u64 as f64).upcast()];
        let bound_fulfill = bind.call(cx, fulfill, bind_args).expect("can call bind()");

        let then = promise.get(cx, "then").expect("then() exists").downcast::<JsFunction>().expect("then() is a function");
        let null = cx.null();
        then.call(cx, promise, vec![bound_fulfill.upcast::<JsValue>(), null.upcast()]).expect("can call then()");

        boxed
    }
}

pub struct JsFutureBuilder<T> {
    future: Pin<Box<JsFuture<T>>>
}

impl <T> JsFutureBuilder<T> {
    pub fn then(self, transform: JsFulfillmentCallback<T>) -> Pin<Box<JsFuture<T>>> {
        let async_context = self.future.info.replace(JsFutureInfo::consumed()).state.into_async_context();
        self.future.info.set(JsFutureInfo::new(transform, async_context));
        self.future
    }
}

trait CurrentContext {
    fn with_context(&self, callback: &mut dyn for<'a> FnMut(&mut FunctionContext<'a>));
}

struct CurrentContextImpl<'a, 'b> {
    context: &'b RefCell<FunctionContext<'a>>
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
        let current_context = self.very_unsafe_current_context.expect("must clean up JsAsyncContext during the fulfillment of the last JS callback invoked");
        current_context.with_context(&mut |cx| {
            let undef = cx.undefined();
            cx.global().set(cx, self.global_key.as_str(), undef).expect("no one else cleared this");
        });
    }
}

// Based on https://crates.io/crates/take_mut.
fn as_ref_cell<T, F>(mut_ref: &mut T, closure: F)
where F: FnOnce(&RefCell<T>) {
    use std::ptr;

    unsafe {
        let cell = RefCell::new(ptr::read(mut_ref));
        let result = panic::catch_unwind(panic::AssertUnwindSafe(|| closure(&cell)));
        ptr::write(mut_ref, cell.into_inner());
        if let Err(err) = result {
            panic::resume_unwind(err);
        }
    }
}

#[derive(Clone, Copy)]
pub struct JsAsyncContextKey<T: neon::types::Value> {
    raw_key: u32,
    _type: PhantomData<T>,
}

#[derive(Clone)]
pub struct JsAsyncContext {
    shared_state: Pin<Rc<RefCell<JsAsyncContextImpl>>>
}

impl JsAsyncContext {
    fn run_with_context(self, cx: &mut FunctionContext, action: impl FnOnce()) {
        // While running, we use a RefCell to dynamically check access to the JS context.
        // But a RefCell has to own its data. as_ref_cell allows us to take the context out of its current reference and put it back later, like a more advanced version of std::mem::replace.
        as_ref_cell(cx, |cx| {
            let c = CurrentContextImpl { context: cx };
            // Lie about the lifetime of `c` so that it can be accessed from arbitrary call sites.
            // "This is advanced, very unsafe Rust!" - std::mem::transmute docs on lifetime extension.
            let opaque_c = unsafe { std::mem::transmute::<_, &'static dyn CurrentContext>(&c as &dyn CurrentContext) };
            let prev_context = self.shared_state.borrow().very_unsafe_current_context;
            self.shared_state.borrow_mut().very_unsafe_current_context = Some(opaque_c);

            action();

            let maybe_pool = self.shared_state.borrow_mut().pool.take();
            if let Some(mut pool) = maybe_pool {
                pool.run_until_stalled();
                assert!(self.shared_state.borrow().complete || self.shared_state.borrow().num_pending_js_futures > 0, "only supports blocking on JavaScript futures");
                self.shared_state.borrow_mut().pool = Some(pool);
            } else {
                // We're in a recursive call and the pool will continue to be processed when we return.
            }

            // If this is the last reference, destroy it while the JavaScript context is still available.
            match Rc::try_unwrap(unsafe { Pin::into_inner_unchecked(self.shared_state) }) {
                Ok(state) => std::mem::drop(state),
                Err(shared_state) => shared_state.borrow_mut().very_unsafe_current_context = prev_context
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
        let result = Self { shared_state: Rc::pin(RefCell::new(JsAsyncContextImpl {
            very_unsafe_current_context: None,
            num_pending_js_futures: 0,
            complete: false,
            pool: Some(LocalPool::new()),
            global_key: String::new(), // replaced below based on the address this object gets pinned to
            num_globals: 0,
            _pinned: PhantomPinned,
        })) };
        result.shared_state.borrow_mut().global_key = format!("__libsignal-client::JsAsyncContext::{:x}", result.shared_state.as_ptr() as usize);
        result
    }

    pub fn with_context<R>(&self, mut callback: impl for<'a> FnMut(&mut FunctionContext<'a>) -> R) -> R {
        let context_holder = self.shared_state.borrow().very_unsafe_current_context.expect("cannot use the JS context outside of a JS call");
        let mut result = None;
        context_holder.with_context(&mut |cx| {
            result = Some(callback(cx))
        });
        result.unwrap() // The callback is always called; we just can't prove it to the compiler.
    }

    pub fn run(self, cx: &mut FunctionContext, future: impl Future<Output = ()> + 'static) {
        let spawner = self.shared_state.borrow().pool.as_ref().expect("should only be called at the top level of an operation").spawner();
        let self_for_future = self.clone();
        spawner.spawn_local(async move {
            future.await;
            self_for_future.shared_state.borrow_mut().complete = true;
        }).expect("can spawn at the top level of an operation");
        self.run_with_context(cx, || {});
    }

    fn context_data_object<'a>(&self, cx: &mut FunctionContext<'a>, create_if_needed: bool) -> Handle<'a, JsObject> {
        let global_key = &self.shared_state.borrow().global_key;

        let context_storage = cx.global().get(cx, global_key.as_ref()).expect("can always get properties on the global object");
        if create_if_needed && context_storage.is_a::<JsUndefined>() {
            let new_storage = cx.empty_object();
            cx.global().set(cx, global_key.as_ref(), new_storage).expect("can always set properties on the global object");
            new_storage
        } else {
            context_storage.downcast().expect("context data accessed improperly somehow")
        }
    }

    pub fn await_promise<T>(&self, mut promise_callback: impl for<'a> FnMut(&mut FunctionContext<'a>) -> Handle<'a, JsObject>) -> JsFutureBuilder<T> {
        let future = self.with_context(|cx| {
            let promise = promise_callback(cx);
            JsFuture::new(cx, promise, self.clone(), |_cx, _handle| panic!("no transform set yet")) // FIXME
        });
        JsFutureBuilder { future }
    }

    pub fn register_context_data<'a, T: neon::types::Value>(&self, cx: &mut FunctionContext<'a>, value: Handle<'a, T>) -> NeonResult<JsAsyncContextKey<T>> {
        let raw_key = self.shared_state.borrow().num_globals;
        self.shared_state.borrow_mut().num_globals += 1;
        self.context_data_object(cx, true).set(cx, raw_key, value)?;
        Ok(JsAsyncContextKey { raw_key, _type: PhantomData })
    }

    pub fn get_context_data<'a, T: neon::types::Value>(&self, cx: &mut FunctionContext<'a>, key: JsAsyncContextKey<T>) -> JsResult<'a, T> {
        self.context_data_object(cx, false).get(cx, key.raw_key)?.downcast_or_throw(cx)
    }
}
