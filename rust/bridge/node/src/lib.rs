//
// Copyright 2020 Signal Messenger, LLC.
// SPDX-License-Identifier: AGPL-3.0-only
//

use libsignal_protocol_rust::*;
use neon::context::Context;
use neon::prelude::*;

mod future;

pub(crate) fn borrow_this<'a, V, T, F>(cx: &mut MethodContext<'a, V>, f: F) -> T
where
    V: Class,
    F: for<'b> FnOnce(neon::borrow::Ref<'b, &mut <V as Class>::Internals>) -> T,
{
    let this = cx.this();
    cx.borrow(&this, f)
}

declare_types! {
    pub class JsPrivateKey for PrivateKey {
        init(_cx) {
            // FIXME: guard against calling this directly
            let mut rng = rand::rngs::OsRng;
            let keypair = KeyPair::generate(&mut rng);
            Ok(keypair.private_key)
        }

        method serialize(mut cx) {
            let bytes = borrow_this(&mut cx, |k| {
                k.serialize()
            });
            // FIXME: check for truncation
            let mut buffer = cx.buffer(bytes.len() as u32)?;
            cx.borrow_mut(&mut buffer, |raw_buffer| {
                raw_buffer.as_mut_slice().copy_from_slice(&bytes);
            });
            Ok(buffer.upcast())
        }
    }
}

fn print_callback_result(mut cx: FunctionContext) -> JsResult<JsValue> {
    let callback = cx.argument::<JsValue>(0)?;
    let done = cx.argument::<JsValue>(1)?;
    let global = cx.global();
    global.set(&mut cx, "__state", callback)?;
    global.set(&mut cx, "__done", done)?;

    let future_context = future::JsFutureExecutionContext::new();

    future::JsFutureExecutionContext::run(future_context.clone(), &mut cx, async move {
        let future = future_context.borrow().with_context(|cx| {
            let callback = cx.global().get(cx, "__state").expect("bleh").downcast().expect("bleeeh");
            future::JsFuture::new(cx, callback, future_context.clone(), |cx3, handle| {
                handle.to_string(cx3).expect("can stringify").value()
            })
        });
        let output: String = future.await;
        future_context.borrow().with_context(|cx| {
            let callback = cx.global().get(cx, "__done").expect("bleh").downcast::<JsFunction>().expect("bleeeh");
            let null = cx.null();
            let args = vec![cx.string(format!("{0} {0}", output))];
            callback.call(cx, null, args).expect("ok");
        });
    });

    Ok(cx.undefined().upcast())
}


struct JsSessionStore {
    // We'd like to store a handle here, but then it'd be locked to the current call context. Instead, we store a hopefully-unique key that lives on the global object.
    key: String,
    context: std::rc::Rc<std::cell::RefCell<future::JsFutureExecutionContext>>,
}

impl JsSessionStore {
    fn new<'a>(cx: &mut FunctionContext<'a>, store: Handle<'a, JsObject>, context: std::rc::Rc<std::cell::RefCell<future::JsFutureExecutionContext>>) -> NeonResult<Self> {
        let key = format!("__store_{:x}", context.as_ptr() as usize);
        assert!(cx.global().get(cx, key.as_str())?.is_a::<JsUndefined>(), "unique key for global storage already in use");
        cx.global().set(cx, key.as_str(), store)?;
        Ok(Self { key, context })
    }

    fn store_object<'a>(&self, cx: &mut FunctionContext<'a>) -> JsResult<'a, JsObject> {
        cx.global().get(cx, self.key.as_str())?.downcast_or_throw(cx)
    }

    async fn perform_a(&self) -> String {
        let future = self.context.borrow().with_context(|cx| {
            let op = self.store_object(cx).expect("exists").get(cx, "a").expect("exists");
            future::JsFuture::new(cx, op.downcast().unwrap(), self.context.clone(), |_cx, handle| {
                handle.downcast::<JsString>().expect("is string").value()
            })
        });
        future.await
    }
}

impl Drop for JsSessionStore {
    fn drop(&mut self) {
        self.context.borrow().with_context(|cx| {
            let undef = cx.undefined();
            cx.global().set(cx, self.key.as_str(), undef).expect("no one else cleared this");
        });
    }
}

async fn use_store_impl(store: JsSessionStore) {
    eprintln!("{}", store.perform_a().await);
}


fn use_store(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let store = cx.argument(0)?;

    let future_context = future::JsFutureExecutionContext::new();

    let store = JsSessionStore::new(&mut cx, store, future_context.clone())?;

    future::JsFutureExecutionContext::run(future_context, &mut cx, async move {
        use_store_impl(store).await;
    });

    Ok(cx.undefined())
}

register_module!(mut cx, {
    cx.export_class::<JsPrivateKey>("PrivateKey")?;
    cx.export_function("print_callback_result", print_callback_result)?;
    cx.export_function("use_store", use_store)?;
    Ok(())
});
