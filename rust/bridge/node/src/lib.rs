//
// Copyright 2020 Signal Messenger, LLC.
// SPDX-License-Identifier: AGPL-3.0-only
//

use libsignal_protocol_rust::*;
use neon::context::Context;
use neon::prelude::*;
use signal_neon_futures::*;

pub(crate) fn borrow_this<'a, V, T, F>(cx: &mut MethodContext<'a, V>, f: F) -> T
where
    V: Class,
    F: FnOnce(neon::borrow::Ref<'_, &mut <V as Class>::Internals>) -> T,
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

    let future_context = JsAsyncContext::new();

    future_context.clone().run(&mut cx, async move {
        let future = future_context.with_context(|cx| {
            let callback = cx
                .global()
                .get(cx, "__state")
                .expect("bleh")
                .downcast()
                .expect("bleeeh");
            JsFuture::new(
                cx,
                callback,
                future_context.clone(),
                |cx3, result| match result {
                    Ok(handle) => handle.to_string(cx3).expect("can stringify").value(),
                    Err(_) => panic!("unexpected JS error"),
                },
            )
        });
        let output: String = future.await;
        future_context.with_context(|cx| {
            let callback = cx
                .global()
                .get(cx, "__done")
                .expect("bleh")
                .downcast::<JsFunction>()
                .expect("bleeeh");
            let null = cx.null();
            let args = vec![cx.string(format!("{0} {0}", output))];
            callback.call(cx, null, args).expect("ok");
        });
    });

    Ok(cx.undefined().upcast())
}

struct JsSessionStore {
    context: JsAsyncContext,
    key: JsAsyncContextKey<JsObject>,
}

impl JsSessionStore {
    fn new<'a>(
        cx: &mut FunctionContext<'a>,
        store: Handle<'a, JsObject>,
        context: JsAsyncContext,
    ) -> NeonResult<Self> {
        let key = context.register_context_data(cx, store)?;
        Ok(Self { context, key })
    }

    async fn perform_a(&self) -> String {
        self.context
            .await_promise(|cx| {
                let store_object = self.context.get_context_data(cx, self.key).expect("exists");
                let op = store_object
                    .get(cx, "a")
                    .expect("exists")
                    .downcast::<JsFunction>()
                    .expect("is function");
                op.call(cx, store_object, std::iter::empty::<Handle<JsValue>>())
                    .expect("success")
                    .downcast()
                    .expect("is object")
            })
            .then(|_cx, result| match result {
                Ok(handle) => handle.downcast::<JsString>().expect("is string").value(),
                Err(handle) => format!(
                    "error: {}",
                    handle.downcast::<JsString>().expect("is string").value()
                ),
            })
            .await
    }
}

async fn use_store_impl(store: JsSessionStore) {
    eprintln!("{}", store.perform_a().await);
}

fn use_store(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let store = cx.argument(0)?;

    let future_context = JsAsyncContext::new();

    let store = JsSessionStore::new(&mut cx, store, future_context.clone())?;

    future_context.run(&mut cx, use_store_impl(store));

    Ok(cx.undefined())
}

register_module!(mut cx, {
    cx.export_class::<JsPrivateKey>("PrivateKey")?;
    cx.export_function("print_callback_result", print_callback_result)?;
    cx.export_function("use_store", use_store)?;
    Ok(())
});
