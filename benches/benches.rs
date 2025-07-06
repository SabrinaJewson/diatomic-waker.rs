use std::{sync::Arc, task::{Wake, Waker}};

use criterion::Criterion;

use diatomic_waker::DiatomicWaker;

fn main() {
    let mut c = Criterion::default().configure_from_args();

    let waker = Waker::from(Arc::new(ExampleWaker(0)));

    c.bench_function("register_same", |b| {
        let mut slot = DiatomicWaker::new();
        slot.sink_ref().register(&waker);
        b.iter(|| slot.sink_ref().register(&waker));
    });

    c.bench_function("register_same_wake", |b| {
        let mut slot = DiatomicWaker::new();
        slot.sink_ref().register(&waker);
        b.iter(|| {
            slot.sink_ref().register(&waker);
            slot.notify();
        });
    });
}

/// A simple waker whose clone operation is not just a no-op.
struct ExampleWaker(#[expect(dead_code)] u8);

impl Wake for ExampleWaker {
    fn wake(self: Arc<Self>) {}
    fn wake_by_ref(self: &Arc<Self>) {}
}
