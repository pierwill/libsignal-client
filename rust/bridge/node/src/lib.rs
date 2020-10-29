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
    let callback = cx.argument(0)?;
    let undefined = cx.undefined();
    let mut spawner = future::JsFutureSpawner::new();
    let future = future::JsFuture::new(cx, callback, &spawner, |cx, handle| {
        handle.to_string(cx).expect("can stringify").value()
    });
    spawner.spawn(async {
        let output: String = future.await;
        eprintln!("{}", output);
    });
    Ok(undefined.upcast())
}

register_module!(mut cx, {
    cx.export_class::<JsPrivateKey>("PrivateKey")?;
    cx.export_function("print_callback_result", print_callback_result)?;
    Ok(())
});
