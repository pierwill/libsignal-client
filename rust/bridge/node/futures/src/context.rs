//
// Copyright 2020 Signal Messenger, LLC.
// SPDX-License-Identifier: AGPL-3.0-only
//

use futures::executor::LocalPool;
use futures::task::LocalSpawnExt;
use neon::prelude::*;
use std::cell::RefCell;
use std::future::Future;
use std::marker::{PhantomData, PhantomPinned};
use std::panic::{self, AssertUnwindSafe};
use std::pin::Pin;
use std::rc::Rc;

use crate::util::*;
use crate::*;

pub struct JsFutureBuilder<T> {
    future: JsFuture<T>,
}

impl<T> JsFutureBuilder<T> {
    pub fn then(
        self,
        transform: impl 'static + for<'a> FnOnce(&'_ mut FunctionContext<'a>, JsPromiseResult<'a>) -> T,
    ) -> JsFuture<T> {
        self.then_try(move |cx, result| Ok(transform(cx, result)))
    }

    pub fn then_try(
        mut self,
        transform: impl 'static
            + for<'a> FnOnce(&mut FunctionContext<'a>, JsPromiseResult<'a>) -> NeonResult<T>,
    ) -> JsFuture<T> {
        self.future.set_transform(transform);
        self.future
    }
}

trait CurrentContext {
    fn with_context(&self, callback: &mut dyn FnMut(&mut FunctionContext<'_>));
}

struct CurrentContextImpl<'a, 'b> {
    context: &'b RefCell<FunctionContext<'a>>,
}

impl<'a, 'b> CurrentContext for CurrentContextImpl<'a, 'b> {
    fn with_context(&self, callback: &mut dyn FnMut(&mut FunctionContext<'_>)) {
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
    pub(crate) fn run_with_context(self, cx: &mut FunctionContext, action: impl FnOnce()) {
        let action = AssertUnwindSafe(action);
        let self_ = AssertUnwindSafe(self);
        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            cx.try_catch(move |cx| {
                // While running, we use a RefCell to dynamically check access to the JS context. But a RefCell has to own its data.
                // as_ref_cell allows us to take the context out of its current reference and put it back later,
                // like a more advanced version of std::mem::replace.
                as_ref_cell(cx, |cx| {
                    let c: &dyn CurrentContext = &CurrentContextImpl { context: cx };
                    // Lie about the lifetime of `c` so that it can be accessed from arbitrary call sites.
                    // "This is advanced, very unsafe Rust!" - std::mem::transmute docs on lifetime extension.
                    #[allow(clippy::transmute_ptr_to_ptr)]
                    let opaque_c = unsafe {
                        std::mem::transmute::<&dyn CurrentContext, &'static dyn CurrentContext>(c)
                    };

                    let AssertUnwindSafe(self_) = self_;
                    let prev_context = self_
                        .shared_state
                        .borrow_mut()
                        .very_unsafe_current_context
                        .replace(opaque_c);

                    let guarded_self = scopeguard::guard(self_, |self_| {
                        // If this is the last reference, destroy it while the JavaScript context is still available.
                        // Otherwise, clean up manually.
                        match Rc::try_unwrap(unsafe {
                            Pin::into_inner_unchecked(self_.shared_state)
                        }) {
                            Ok(state) => std::mem::drop(state),
                            Err(shared_state) => {
                                shared_state.borrow_mut().very_unsafe_current_context =
                                    prev_context;
                            }
                        };
                    });

                    action();

                    let maybe_pool = guarded_self.shared_state.borrow_mut().pool.take();
                    if let Some(mut pool) = maybe_pool {
                        pool.run_until_stalled();
                        let mut shared_state = guarded_self.shared_state.borrow_mut();
                        assert!(
                            shared_state.complete || shared_state.num_pending_js_futures > 0,
                            "only supports blocking on JavaScript futures"
                        );
                        shared_state.pool = Some(pool);
                    } else {
                        // We're in a recursive call and the pool will continue to be processed when we return.
                    }
                });

                // Dummy return value; see https://github.com/neon-bindings/neon/issues/630
                Ok(cx.undefined())
            })
        }));

        match result {
            Ok(Ok(_)) => return,
            Ok(Err(js_err)) => {
                // Older versions of Node drop thrown exceptions on the ground if they occur on the microtask queue
                // (e.g. when evaluating a promise).
                // So, force an abort instead to preserve safety.
                eprintln!(
                    "\n!! Thrown errors must be handled during async execution !!\n{}\n",
                    js_err
                        .to_string(cx)
                        .expect("could not stringify thrown error")
                        .value()
                );
            }
            Err(panic) => {
                // Neon translates unwinding panics into JavaScript exceptions,
                // but those will get dropped (see above).
                eprintln!(
                    "\n!! Panics must be handled during async execution !!\n{}\n",
                    describe_any(&panic)
                );
            }
        }
        std::process::abort()
    }

    pub(crate) fn register_future(&self) {
        self.shared_state.borrow_mut().num_pending_js_futures += 1
    }

    pub(crate) fn resolve_future(&self) {
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
            "__signal_neon_futures::JsAsyncContext({:x})",
            result.shared_state.as_ptr() as usize
        );
        result
    }

    pub fn with_context<R>(&self, callback: impl FnOnce(&mut FunctionContext<'_>) -> R) -> R {
        let context_holder = self
            .shared_state
            .borrow()
            .very_unsafe_current_context
            .expect("cannot use the JS context outside of a JS call");
        let mut result = None;
        let mut callback = Some(callback);
        context_holder.with_context(&mut |cx| {
            let callback = callback.take().unwrap();
            result = Some(callback(cx));
        });
        result.unwrap() // The callback is always called; we just can't prove it to the compiler.
    }

    pub(crate) fn try_catch<'a, R>(
        &self,
        cx: &mut FunctionContext<'a>,
        callback: impl FnOnce(&mut FunctionContext<'a>) -> NeonResult<R>,
    ) -> Result<R, JsAsyncContextKey<JsValue>> {
        let mut result = None;
        let success_or_exception = {
            let mut result = AssertUnwindSafe(&mut result);
            let callback = AssertUnwindSafe(callback);
            cx.try_catch(move |cx| {
                **result = Some(callback.0(cx)?);
                Ok(cx.undefined())
            })
        };
        match success_or_exception {
            Ok(_) => Ok(result.unwrap()),
            Err(exception) => Err(self.register_context_data(cx, exception)),
        }
    }

    pub fn try_with_context<R>(
        &self,
        callback: impl FnOnce(&mut FunctionContext<'_>) -> NeonResult<R>,
    ) -> Result<R, JsAsyncContextKey<JsValue>> {
        self.with_context(move |cx| self.try_catch(cx, callback))
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

    pub fn await_promise<T>(
        &self,
        mut promise_callback: impl for<'a> FnMut(&mut FunctionContext<'a>) -> JsResult<'a, JsObject>,
    ) -> JsFutureBuilder<T> {
        let future = self
            .try_with_context(|cx| {
                let promise = promise_callback(cx)?;
                Ok(JsFuture::new(cx, promise, self.clone(), |_cx, _handle| {
                    panic!("no transform set yet")
                }))
            })
            .unwrap_or_else(JsFuture::err);
        JsFutureBuilder { future }
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

    pub fn register_context_data<'a, T: neon::types::Value>(
        &self,
        cx: &mut FunctionContext<'a>,
        value: Handle<'a, T>,
    ) -> JsAsyncContextKey<T> {
        let raw_key = self.shared_state.borrow().num_globals;
        self.shared_state.borrow_mut().num_globals += 1;
        self.context_data_object(cx, true)
            .set(cx, raw_key, value)
            .expect("setting value on private object");
        JsAsyncContextKey {
            raw_key,
            _type: PhantomData,
        }
    }

    pub fn get_context_data<'a, T: neon::types::Value>(
        &self,
        cx: &mut FunctionContext<'a>,
        key: JsAsyncContextKey<T>,
    ) -> Handle<'a, T> {
        self.context_data_object(cx, false)
            .get(cx, key.raw_key)
            .expect("invalid key")
            .downcast()
            .expect("type has not changed")
    }
}

impl Default for JsAsyncContext {
    fn default() -> Self {
        Self::new()
    }
}
