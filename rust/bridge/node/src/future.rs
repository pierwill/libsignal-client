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
use std::marker::PhantomPinned;
use std::panic;
use futures::executor::LocalPool;
use futures::task::LocalSpawnExt;
use scopeguard::defer;

type JsFulfillmentCallback<T> = for<'a> fn(&mut FunctionContext<'a>, Handle<'a, JsValue>) -> T;
type OpaqueJsFulfillmentCallback = for<'a> fn(&mut FunctionContext<'a>, Handle<'a, JsValue>, *const());

enum JsFutureState<T> {
    Started(Rc<RefCell<JsFutureExecutionContext>>),
    Waiting(Rc<RefCell<JsFutureExecutionContext>>, Waker),
    Complete(T),
    Consumed,
}

impl<T> JsFutureState<T> {
    fn into_executor(self) -> Rc<RefCell<JsFutureExecutionContext>> {
        match self {
            JsFutureState::Started(executor) | JsFutureState::Waiting(executor, _) => executor,
            JsFutureState::Complete(_) | JsFutureState::Consumed => panic!("already completed"),
        }
    }
}

struct JsFutureInfo<T> {
    transform: JsFulfillmentCallback<T>,
    state: JsFutureState<T>,
}

impl<T> JsFutureInfo<T> {
    fn new(transform: JsFulfillmentCallback<T>, executor: Rc<RefCell<JsFutureExecutionContext>>) -> Self {
        Self {
            transform,
            state: JsFutureState::Started(executor),
        }
    }

    fn waiting_on(self, waker: Waker) -> Self {
        Self {
            transform: self.transform,
            state: JsFutureState::Waiting(self.state.into_executor(), waker),
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

    fn poll(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        let info = self.info.replace(JsFutureInfo::consumed());
        match info.state {
            JsFutureState::Complete(result) => return Poll::Ready(result),
            JsFutureState::Consumed => panic!("already consumed"),
            JsFutureState::Started(ref executor) => {
                executor.borrow_mut().register_future();
            }
            JsFutureState::Waiting(_, _) => {}
        }
        self.info.set(info.waiting_on(cx.waker().clone()));
        Poll::Pending
    }
}

fn fulfill<'a>(mut cx: FunctionContext<'a>) -> JsResult<'a, JsUndefined> {
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
            JsFutureState::Started(executor) => {
                executor.borrow_mut().resolve_future();
                JsFutureExecutionContext::run_with_context(&executor, cx, || {});
            }
            JsFutureState::Waiting(executor, waker) => {
                executor.borrow_mut().resolve_future();
                JsFutureExecutionContext::run_with_context(&executor, cx, || waker.wake());
            }
            _ => {
                panic!("already fulfilled")
            }
        }
    }
}

impl<T> JsFuture<T> {
    pub fn new<'a, C>(cx: &mut C, promise: Handle<'a, JsObject>, executor: Rc<RefCell<JsFutureExecutionContext>>, transform: JsFulfillmentCallback<T>) -> Pin<Box<Self>> where C: Context<'a> {
        let cell = Cell::new(JsFutureInfo::new(transform, executor));
        let boxed = Box::pin(Self { info: cell, _pinned: PhantomPinned });
        let boxed_ptr = &(*boxed) as *const Self;
        let save_result_ptr = unsafe { std::mem::transmute::<_, *const ()>(get_save_result_fn::<T>()) };

        let fulfill = JsFunction::new(cx, fulfill).expect("can create function");
        let bind = fulfill.get(cx, "bind").expect("bind() exists").downcast::<JsFunction>().expect("bind() is a function");
        let bind_args = vec![cx.undefined().upcast::<JsValue>(), cx.number(save_result_ptr as u64 as f64).upcast(), cx.number(boxed_ptr as u64 as f64).upcast()];
        let bound_fulfill = bind.call(cx, fulfill, bind_args).expect("can call bind()");

        let then = promise.get(cx, "then").expect("then() exists").downcast::<JsFunction>().expect("then() is a function");
        let undefined = cx.undefined();
        then.call(cx, promise, vec![bound_fulfill.upcast::<JsValue>(), undefined.upcast()]).expect("can call then()");

        boxed
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

pub struct JsFutureExecutionContext {
    // This 'static is a lie that allows the JavaScript context to be accessed from arbitrary use sites.
    // Be very very careful that you do not persist this reference.
    // Also do not share the 'static outside the implementation of JsFutureExecutionContext.
    very_unsafe_current_context: Option<&'static dyn CurrentContext>,

    num_pending_js_futures: i32,

    complete: bool,

    pool: Option<LocalPool>,
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


impl JsFutureExecutionContext {
    fn run_with_context<'a>(self_: &RefCell<Self>, cx: &mut FunctionContext<'a>, action: impl FnOnce()) {
        // While running, we use a RefCell to dynamically check access to the JS context.
        // But a RefCell has to own its data. take_mut allows us to take the context out of its current reference and put it back later, like a more advanced version of std::mem::replace.
        as_ref_cell(cx, |cx| {
            let c = CurrentContextImpl { context: cx };
            // Lie about the lifetime of `c` so that it can be accessed from arbitrary call sites.
            // "This is advanced, very unsafe Rust!" - std::mem::transmute docs on lifetime extension.
            let opaque_c = unsafe { std::mem::transmute::<_, &'static dyn CurrentContext>(&c as &dyn CurrentContext) };
            let prev_context = self_.borrow().very_unsafe_current_context;
            self_.borrow_mut().very_unsafe_current_context = Some(opaque_c);
            defer! { self_.borrow_mut().very_unsafe_current_context = prev_context };

            action();

            let maybe_pool = { self_.borrow_mut().pool.take() };
            if let Some(mut pool) = maybe_pool {
                pool.run_until_stalled();
                assert!(self_.borrow().complete || self_.borrow().num_pending_js_futures > 0, "only supports blocking on JavaScript futures");
                self_.borrow_mut().pool = Some(pool);
            } else {
                // We're in a recursive call and the pool will continue to be processed when we return.
            }
        });
    }

    fn register_future(&mut self) {
        self.num_pending_js_futures += 1
    }

    fn resolve_future(&mut self) {
        self.num_pending_js_futures -= 1
    }

    pub fn with_context<R>(&self, mut callback: impl for<'a> FnMut(&mut FunctionContext<'a>) -> R) -> R {
        let context_holder = self.very_unsafe_current_context.expect("cannot use the JS context outside of a JS call");
        let mut result = None;
        context_holder.with_context(&mut |cx| {
            result = Some(callback(cx))
        });
        result.unwrap() // The callback is always called; we just can't prove it to the compiler.
    }

    pub fn new() -> Rc<RefCell<Self>> {
        Rc::new(RefCell::new(Self { very_unsafe_current_context: None, num_pending_js_futures: 0, complete: true, pool: Some(LocalPool::new()) }))
    }

    pub fn run<'a>(self_: Rc<RefCell<Self>>, cx: &mut FunctionContext<'a>, future: impl Future<Output = ()> + 'static) {
        let self_for_future = self_.clone();
        self_.borrow().pool.as_ref().unwrap().spawner().spawn_local(async move {
            future.await;
            self_for_future.borrow_mut().complete = true;
        }).expect("cannot fail to spawn on a LocalPool");
        Self::run_with_context(&self_, cx, || {});
    }
}
