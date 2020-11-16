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

use crate::future_builder::JsFutureBuilder;
use crate::util::*;
use crate::*;

/// Abstraction over a Neon [FunctionContext] that can be accessed dynamically.
///
/// Specifically, this erases the lifetime present in a [FunctionContext].
///
/// Not intended to be used directly; it's part of the implementation of [JsAsyncContext::with_context].
trait CurrentContext {
    fn with_context(&self, callback: &mut dyn FnMut(&mut FunctionContext<'_>));
}

/// A concrete [CurrentContext] implemented using a borrowed RefCell.
struct CurrentContextImpl<'a, 'b> {
    context: RefCell<&'b mut FunctionContext<'a>>,
}

impl<'a, 'b> CurrentContext for CurrentContextImpl<'a, 'b> {
    fn with_context(&self, callback: &mut dyn FnMut(&mut FunctionContext<'_>)) {
        callback(&mut self.context.borrow_mut())
    }
}

/// The shared state in a [JsAsyncContext]. See there for more details.
struct JsAsyncContextImpl {
    /// Used to implement [JsAsyncContext::with_context].
    ///
    /// This `'static` is a lie that allows the JavaScript context to be accessed from arbitrary use sites.
    /// Be very very careful that you do not persist this reference.
    /// Also do not share the `'static` outside the implementation of JsAsyncContextImpl.
    very_unsafe_current_context: Option<&'static dyn CurrentContext>,

    /// The executor for the top-level future that's run in this context.
    ///
    /// Will be [None] during re-entrant execution.
    pool: Option<LocalPool>,

    /// Used to guard against awaiting *non*-JavaScript futures, which is not supported.
    num_pending_js_futures: i32,
    /// Tracks when the top-level future has completed.
    complete: bool,

    /// A unique string used to store data in the global JavaScript context.
    global_key: String,

    /// The next value to use as a [JsAsyncContextKey] when registering context data.
    next_global_id: u32,

    /// Pins the context in place so that it has a stable address.
    ///
    /// This is used in the generation of `global_key`.
    _pinned: PhantomPinned,
}

impl Drop for JsAsyncContextImpl {
    fn drop(&mut self) {
        let current_context = self
            .very_unsafe_current_context
            .expect("must not persist JsAsyncContext beyond the lifetime of the future being run");
        current_context.with_context(&mut |cx| {
            let undef = cx.undefined();
            cx.global()
                .set(cx, self.global_key.as_str(), undef)
                .expect("no one else cleared this");
        });
    }
}

/// Identifies some data stored "in" a [JsAsyncContext].
///
/// [JsAsyncContext::register_context_data] and [JsAsyncContext::get_context_data] allow you to use JavaScript values across different [FunctionContexts](type@FunctionContext).
/// JsAsyncContextKey is the "claim check" for that data.
///
/// A JsAsyncContextKey can only be used with the JsAsyncContext it came from (or a clone thereof).
#[derive(Clone, Copy)]
pub struct JsAsyncContextKey<T: neon::types::Value> {
    /// Used to verify uniqueness.
    context_id: *const (),

    /// Uniquely identifies the data being stored.
    raw_key: u32,

    /// Use the `T` parameter without claiming we actually own an instance thereof.
    ///
    /// See <https://doc.rust-lang.org/std/marker/struct.PhantomData.html#ownership-and-the-drop-check>.
    _type: PhantomData<*const T>,
}

/// An execution context for [JsFutures](struct@JsFuture).
///
/// JsAsyncContext's primary purpose is to allow a future (such as an `async` block) to run while blocking on JavaScript promises.
/// You can do this using the [run](fn@JsAsyncContext::run) method, or the (freestanding) [promise](fn@crate::promise) function.
///
/// The actual adapter for a JavaScript promise is [JsFuture],
/// but in most cases it's more convenient to create one using [await_promise](fn@JsAsyncContext::await_promise).
/// See [JsFutureBuilder] for more details.
///
/// If you are *not* going to await a promise, you can get access to the current Neon [FunctionContext] using [with_context](fn@JsAsyncContext::with_context).
///
/// Cloning a JsAsyncContext shares the same context; it does not make a fresh one.
#[derive(Clone)]
pub struct JsAsyncContext {
    shared_state: Pin<Rc<RefCell<JsAsyncContextImpl>>>,
}

impl JsAsyncContext {
    /// Sets `cx` as the current context and runs `action`, then advances the main future until it stalls again.
    ///
    /// Neither `action` nor the main future is permitted to throw any unhandled JavaScript exceptions here;
    /// because this is usually called from the Node microtask queue (e.g. when a promise was fulfilled),
    /// there is nothing to handle them.
    /// The [promise](fn@crate::promise) function forwards uncaught exceptions as rejections,
    /// but clients that don't use that infrastructure must handle them on their own.
    ///
    /// If `self` is the last reference to this context's shared state, that state is destroyed before exiting `cx`.
    ///
    /// The implementation of this function is full of a bunch of Rust black magic that basically sidesteps the compiler's static safety.
    /// Be very careful when changing it.
    pub(crate) fn run_with_context<'a>(
        self,
        mut cx: &mut FunctionContext<'a>,
        action: impl FnOnce(&mut FunctionContext<'a>),
    ) {
        let cx_ref = &mut cx;
        // Unwinding is safe here because we will immediately abort if we actually unwind (see below).
        let result = panic::catch_unwind(AssertUnwindSafe(move || {
            // These shouldn't actually be necessary.
            // try_catch only propagates unwinds; it does not resume arbitrary execution after an unwind.
            let action = AssertUnwindSafe(action);
            let self_ = AssertUnwindSafe(self);
            cx_ref.try_catch(move |cx| {
                // First run the action, which wants exclusive synchronous use of cx.
                action.0(cx);

                // Next, set up the wrapper so JsAsyncContext::with_context can access cx dynamically.
                let c = &CurrentContextImpl {
                    context: RefCell::new(cx),
                };
                // Lie about the lifetime of 'c' so that it can be accessed from arbitrary call sites.
                // "This is advanced, very unsafe Rust!" - std::mem::transmute docs on lifetime extension.
                #[allow(clippy::transmute_ptr_to_ptr)]
                let opaque_c = unsafe {
                    std::mem::transmute::<&dyn CurrentContext, &'static dyn CurrentContext>(c)
                };

                // Swap out the previous context.
                let prev_context = self_
                    .0
                    .shared_state
                    .borrow_mut()
                    .very_unsafe_current_context
                    .replace(opaque_c);

                let guarded_self = scopeguard::guard(self_.0, |self_| {
                    // If this is the last reference, destroy it while the JavaScript context is still available.
                    // Otherwise, clean up manually.
                    match Rc::try_unwrap(unsafe { Pin::into_inner_unchecked(self_.shared_state) }) {
                        Ok(state) => std::mem::drop(state),
                        Err(shared_state) => {
                            shared_state.borrow_mut().very_unsafe_current_context = prev_context;
                        }
                    };
                });

                // Advance the top-level future until it gets blocked again.
                // Note the lifetime dance here: we avoid borrowing shared_state while running the pool, and then borrow it again to put the pool back.
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

                // Dummy return value; see https://github.com/neon-bindings/neon/issues/630
                Ok(cx.undefined())
            })
        }));

        // Okay, we've finished! But was there an exception or panic?
        // Neon translates unwinding panics into JavaScript exceptions,
        // but that doesn't help us in an asynchronous context.
        // To preserve safety and catch bugs, we print the error and abort.
        match result {
            Ok(Ok(_)) => return,
            Ok(Err(js_err)) => {
                eprintln!(
                    "\n!! Thrown errors must be handled during async execution !!\n{}\n",
                    js_err
                        .to_string(cx)
                        .expect("could not stringify thrown error")
                        .value()
                );
            }
            Err(panic) => {
                eprintln!(
                    "\n!! Panics must be handled during async execution !!\n{}\n",
                    describe_any(&panic)
                );
            }
        }
        std::process::abort()
    }

    /// Tracks outstanding futures to make sure there's always something to block on.
    pub(crate) fn register_future(&self) {
        self.shared_state.borrow_mut().num_pending_js_futures += 1
    }

    /// Notes that a future has been fulfilled.
    ///
    /// See also [register_future].
    pub(crate) fn fulfill_future(&self) {
        self.shared_state.borrow_mut().num_pending_js_futures -= 1
    }

    /// Returns the unique, pinned address associated with this context.
    ///
    /// Used for context data.
    fn opaque_id(&self) -> *const () {
        self.shared_state.as_ptr() as *const ()
    }

    /// Initializes a fresh context.
    fn new() -> Self {
        let result = Self {
            shared_state: Rc::pin(RefCell::new(JsAsyncContextImpl {
                very_unsafe_current_context: None,
                num_pending_js_futures: 0,
                complete: false,
                pool: Some(LocalPool::new()),
                global_key: String::new(), // replaced below based on the address this object gets pinned to
                next_global_id: 0,
                _pinned: PhantomPinned,
            })),
        };
        result.shared_state.borrow_mut().global_key = format!(
            "__signal_neon_futures::JsAsyncContext({:p})",
            result.opaque_id()
        );
        result
    }

    /// Runs `callback` in the current active [FunctionContext].
    ///
    /// This can be used to perform JavaScript actions after an `await`.
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

    /// A wrapper around [Context::try_catch] that persists any thrown exception using [register_context_data](fn@Self::register_context_data).
    pub fn try_catch<'a, R>(
        &self,
        cx: &mut FunctionContext<'a>,
        callback: impl FnOnce(&mut FunctionContext<'a>) -> NeonResult<R>,
    ) -> Result<R, JsAsyncContextKey<JsValue>> {
        let mut result = None;
        let success_or_exception = {
            // These shouldn't actually be necessary.
            // try_catch only propagates unwinds; it does not resume arbitrary execution after an unwind.
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

    /// The main entry point for JsAsyncContext, if not using [promise](fn@crate::promise).
    ///
    /// Given a function that *produces* a future (probably an `async` block),
    /// runs that function with a fresh JsAsyncContext and then starts the resulting future,
    /// which will run to completion synchronously via callbacks from JavaScript.
    ///
    /// If `callback` fails to return a future because of a JavaScript exception,
    /// that exception will be propagated to the (synchronous) caller.
    pub fn run<'a, F>(
        cx: &mut FunctionContext<'a>,
        callback: impl FnOnce(&mut FunctionContext<'a>, JsAsyncContext) -> NeonResult<F>,
    ) -> NeonResult<()>
    where
        F: Future<Output = ()> + 'static,
    {
        let async_context = Self::new();
        let spawner = async_context
            .shared_state
            .borrow()
            .pool
            .as_ref()
            .unwrap()
            .spawner();
        let mut error_to_throw = None;
        async_context.clone().run_with_context(cx, |cx| {
            let mut future = None;
            let maybe_error = {
                let callback = AssertUnwindSafe(callback);
                let mut future_ref = AssertUnwindSafe(&mut future);
                let async_context = AssertUnwindSafe(async_context.clone());
                cx.try_catch(move |cx| {
                    **future_ref = Some(AssertUnwindSafe(callback.0(cx, async_context.0)?));

                    // Dummy return value; see https://github.com/neon-bindings/neon/issues/630
                    Ok(cx.undefined())
                })
            };
            match maybe_error {
                Ok(_) => {
                    spawner
                        .spawn_local(async move {
                            future.unwrap().0.await;
                            async_context.shared_state.borrow_mut().complete = true;
                        })
                        .expect("can spawn at the top level of an operation");
                }
                Err(error) => error_to_throw = Some(error),
            }
        });

        if let Some(error) = error_to_throw {
            cx.throw(error)?;
        }
        Ok(())
    }

    /// Given a function that produces a JavaScript Promise, calls that function and prepares to wait on the result.
    ///
    /// This is useful when you want to call an `async` JavaScript function and wait on the result.
    /// The handling of the result (whether success or failure) is configured using the returned [JsFutureBuilder].
    ///
    /// If `promise_callback` fails, that failure will be treated as a rejection to be processed as the promise's result.
    /// If you want to handle such failures synchronously, you can use [with_context](fn@Self::with_context) instead and manually create a [JsFuture].
    pub fn await_promise<T>(
        &self,
        mut promise_callback: impl for<'a> FnMut(&mut FunctionContext<'a>) -> JsResult<'a, JsObject>,
    ) -> JsFutureBuilder<T> {
        let future = self.try_with_context(|cx| {
            let promise = promise_callback(cx)?;
            Ok(JsFuture::new(cx, promise, self.clone(), |_cx, _handle| {
                panic!("no transform set yet")
            }))
        });
        JsFutureBuilder {
            async_context: self,
            state: future,
        }
    }

    /// Accesses the single global object that stores all of this context's data.
    fn context_data_object<'a>(
        &self,
        cx: &mut FunctionContext<'a>,
        create_if_needed: bool,
    ) -> Handle<'a, JsArray> {
        let global_key = &self.shared_state.borrow().global_key;

        let context_storage = cx
            .global()
            .get(cx, global_key.as_ref())
            .expect("can always get properties on the global object");
        if create_if_needed && context_storage.is_a::<JsUndefined>() {
            let new_storage = cx.empty_array();
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

    /// Stores `value` for the lifetime of this JsAsyncContext.
    ///
    /// This allows you to pass data across [FunctionContexts](type@FunctionContext) and thus across `await` boundaries.
    ///
    /// The value can be accessed again using [get_context_data](fn@Self::get_context_data).
    pub fn register_context_data<'a, T: neon::types::Value>(
        &self,
        cx: &mut FunctionContext<'a>,
        value: Handle<'a, T>,
    ) -> JsAsyncContextKey<T> {
        let raw_key = self.shared_state.borrow().next_global_id;
        self.shared_state.borrow_mut().next_global_id += 1;
        self.context_data_object(cx, true)
            .set(cx, raw_key, value)
            .expect("setting value on private object");
        JsAsyncContextKey {
            context_id: self.opaque_id(),
            raw_key,
            _type: PhantomData,
        }
    }

    /// Retrieves a value previously stored with [register_context_data](fn@Self::register_context_data).
    ///
    /// This allows you to pass data across [FunctionContexts](type@FunctionContext) and thus across `await` boundaries.
    pub fn get_context_data<'a, T: neon::types::Value>(
        &self,
        cx: &mut FunctionContext<'a>,
        key: JsAsyncContextKey<T>,
    ) -> Handle<'a, T> {
        assert!(
            key.context_id == self.opaque_id(),
            "Key used for a different JsAsyncContext"
        );
        self.context_data_object(cx, false)
            .get(cx, key.raw_key)
            .expect("invalid key")
            .downcast()
            .expect("type has not changed")
    }
}
