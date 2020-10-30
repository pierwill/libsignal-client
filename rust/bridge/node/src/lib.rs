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

fn print_callback_result<'a>(mut cx: FunctionContext<'a>) -> JsResult<JsValue> {
    let callback = cx.argument::<JsValue>(0)?;
    let global = cx.global();
    global.set(&mut cx, "__state", callback)?;

    let future_context = future::JsFutureExecutionContext::new();

    future::JsFutureExecutionContext::run(&future_context.clone(), &mut cx, async move {
        let future = future_context.borrow().with_context(|cx| {
            let callback = cx.global().get(cx, "__state").expect("bleh").downcast().expect("bleeeh");
            future::JsFuture::new(cx, callback, future_context.clone(), |cx3, handle| {
                handle.to_string(cx3).expect("can stringify").value()
            })
        });
        let output: String = future.await;
        eprintln!("{}", output);
    });

    Ok(cx.undefined().upcast())
}

register_module!(mut cx, {
    cx.export_class::<JsPrivateKey>("PrivateKey")?;
    cx.export_function("print_callback_result", print_callback_result)?;
    Ok(())
});
