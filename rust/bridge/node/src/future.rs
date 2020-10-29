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
use std::cell::Cell;
use futures::executor::LocalPool;
use futures::task::LocalSpawnExt;

type JsFulfillmentCallback<T> = for<'a> fn(&mut FunctionContext<'a>, Handle<'a, JsValue>) -> T;
type OpaqueJsFulfillmentCallback = for<'a> fn(&mut FunctionContext<'a>, Handle<'a, JsValue>, *const());

enum JsFutureState<T> {
    Started,
    Waiting(Waker),
    Complete(T),
    Consumed,
}

struct JsFutureInfo<T> {
    transform: JsFulfillmentCallback<T>,
    state: JsFutureState<T>,
    pool: *mut LocalPool,
}

impl<T> JsFutureInfo<T> {
    fn new(transform: JsFulfillmentCallback<T>, pool: &LocalPool) -> Self {
        Self {
            transform,
            state: JsFutureState::Started,
            pool: pool as *const LocalPool as *mut LocalPool,
        }
    }

    fn waiting_on(self, waker: Waker) -> Self {
        Self {
            transform: self.transform,
            state: JsFutureState::Waiting(waker),
            pool: self.pool,
        }
    }

    fn complete(self, value: T) -> Self {
        Self {
            transform: |_cx, _handle| panic!("already completed"),
            state: JsFutureState::Complete(value),
            pool: std::ptr::null_mut(),
        }
    }

    fn consumed() -> Self {
        Self {
            transform: |_cx, _handle| panic!("already consumed"),
            state: JsFutureState::Consumed,
            pool: std::ptr::null_mut(),
        }
    }

    fn waker_or_none(&self) -> Option<Waker> {
        if let JsFutureState::Waiting(waker) = &self.state {
            Some(waker.clone())
        } else {
            None
        }
    }
}


pub struct JsFuture<T> {
    info: Box<Cell<JsFutureInfo<T>>>,
}

impl <T> Future for JsFuture<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        let info = self.info.replace(JsFutureInfo::consumed());
        if let JsFutureState::Complete(result) = info.state {
            Poll::Ready(result)
        } else {
            self.info.set(info.waiting_on(cx.waker().clone()));
            Poll::Pending
        }
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
    return |cx, js_result, opaque_cell_ptr| {
        let cell = unsafe { (opaque_cell_ptr as *const Cell<JsFutureInfo<T>>).as_ref().unwrap() };
        let info = cell.replace(JsFutureInfo::consumed());

        let waker = info.waker_or_none();
        let pool = info.pool;

        let result = (info.transform)(cx, js_result);
        cell.set(info.complete(result));

        if let Some(waker) = waker {
            waker.wake();
        }
        unsafe { (*pool).run_until_stalled() }
    }
}

impl<T> JsFuture<T> {
    pub fn new<'a, C>(mut cx: C, promise: Handle<'a, JsObject>, spawner: &JsFutureSpawner, transform: JsFulfillmentCallback<T>) -> Self where C: Context<'a> {
        let cell = Box::new(Cell::new(JsFutureInfo::new(transform, &*spawner.pool)));
        let cell_ptr = cell.as_ptr();
        let save_result_ptr = unsafe { std::mem::transmute::<_, *const ()>(get_save_result_fn::<T>()) };

        let fulfill = JsFunction::new(&mut cx, fulfill).expect("can create function");
        let bind = fulfill.get(&mut cx, "bind").expect("bind() exists").downcast::<JsFunction>().expect("bind() is a function");
        let bind_args = vec![cx.undefined().upcast::<JsValue>(), cx.number(save_result_ptr as u64 as f64).upcast(), cx.number(cell_ptr as u64 as f64).upcast()];
        let bound_fulfill = bind.call(&mut cx, fulfill, bind_args).expect("can call bind()");

        let then = promise.get(&mut cx, "then").expect("then() exists").downcast::<JsFunction>().expect("then() is a function");
        let undefined = cx.undefined();
        then.call(&mut cx, promise, vec![bound_fulfill.upcast::<JsValue>(), undefined.upcast()]).expect("can call then()");

        Self { info: cell }
    }
}

// const jsWakerVTable: std::task::RawWakerVTable = std::task::RawWakerVTable::new(
//     |p| RawWaker::new(p, &jsWakerVTable),
//     |p| {
//         let pool = p as *mut LocalPool;
//         unsafe { (*pool).run_until_stalled() }
//     },
//     |p| {
//         let pool = p as *mut LocalPool;
//         unsafe { (*pool).run_until_stalled() }
//     },
//     |_p| {}
// );

pub struct JsFutureSpawner {
    pool: &'static mut LocalPool
}

impl JsFutureSpawner {
    pub fn new() -> Self {
        Self { pool: Box::leak(Box::new(LocalPool::new())) }
    }

    pub fn spawn(&mut self, future: impl Future<Output = ()> + 'static) {
        self.pool.spawner().spawn_local(future).unwrap();
        self.pool.run_until_stalled();
    }
}
