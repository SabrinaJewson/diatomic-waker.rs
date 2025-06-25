use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

use crate::WakeSinkRef;
use crate::{impl_atomic, impl_lockfree};

/// A primitive that can send or await notifications.
///
/// It is almost always preferable to use the [`WakeSink`](crate::WakeSink) and
/// [`WakeSource`](crate::WakeSource) which offer more convenience at the cost
/// of an allocation in an `Arc`.
///
/// If allocation is not possible or desirable, the
/// [`sink_ref`](DiatomicWaker::sink_ref) method can be used to create a
/// [`WakeSinkRef`] handle and one or more
/// [`WakeSourceRef`](crate::borrowed_waker::WakeSourceRef)s, the non-owned
/// counterparts to `WakeSink` and `WakeSource`.
///
/// Finally, `DiatomicWaker` exposes `unsafe` methods that can be used to create
/// custom synchronization primitives.
#[derive(Debug)]
pub struct DiatomicWaker {
    atomic: [impl_atomic::DiatomicWaker; impl_atomic::ENABLE as usize],
    lock_free: [impl_lockfree::DiatomicWaker; (!impl_atomic::ENABLE) as usize],
}

impl DiatomicWaker {
    /// Creates a new `DiatomicWaker`.
    #[cfg(not(all(test, diatomic_waker_loom)))]
    pub const fn new() -> Self {
        Self {
            atomic: impl_atomic::DiatomicWaker::new(),
            lock_free: [const { impl_lockfree::DiatomicWaker::new() };
                (!impl_atomic::ENABLE) as usize],
        }
    }

    #[cfg(all(test, diatomic_waker_loom))]
    pub fn new() -> Self {
        Self {
            atomic: [],
            lock_free: [crate::impl_lockfree::DiatomicWaker::new()],
        }
    }

    /// Returns a sink with a lifetime bound to this `DiatomicWaker`.
    ///
    /// This mutably borrows the waker, thus ensuring that at most one
    /// associated sink can be active at a time.
    pub fn sink_ref(&mut self) -> WakeSinkRef<'_> {
        WakeSinkRef { inner: self }
    }

    /// Sends a notification if a waker is registered.
    ///
    /// This automatically unregisters any waker that may have been previously
    /// registered.
    #[expect(clippy::out_of_bounds_indexing)]
    pub fn notify(&self) {
        if impl_atomic::ENABLE {
            self.atomic[0].notify();
        } else {
            self.lock_free[0].notify();
        }
    }

    /// Registers a new waker.
    ///
    /// Registration is lazy: the waker is cloned only if it differs from the
    /// last registered waker (note that the last registered waker is cached
    /// even if it was unregistered).
    ///
    /// # Safety
    ///
    /// The `register`, `unregister` and `wait_until` methods cannot be used
    /// concurrently from multiple threads.
    #[expect(clippy::out_of_bounds_indexing)]
    pub unsafe fn register(&self, waker: &Waker) {
        if impl_atomic::ENABLE {
            self.atomic[0].register(waker);
        } else {
            self.lock_free[0].register(waker);
        }
    }

    /// Unregisters the waker.
    ///
    /// After the waker is unregistered, subsequent calls to `notify` will be
    /// ignored.
    ///
    /// Note that the previously-registered waker (if any) remains cached.
    ///
    /// # Safety
    ///
    /// The `register`, `unregister` and `wait_until` methods cannot be used
    /// concurrently from multiple threads.
    #[expect(clippy::out_of_bounds_indexing)]
    pub unsafe fn unregister(&self) {
        if impl_atomic::ENABLE {
            self.atomic[0].unregister();
        } else {
            self.lock_free[0].unregister();
        }
    }

    /// Returns a future that can be `await`ed until the provided predicate
    /// returns a value.
    ///
    /// The predicate is checked each time a notification is received.
    ///
    /// # Safety
    ///
    /// The `register`, `unregister` and `wait_until` methods cannot be used
    /// concurrently from multiple threads.
    pub unsafe fn wait_until<P, T>(&self, predicate: P) -> WaitUntil<'_, P, T>
    where
        P: FnMut() -> Option<T>,
    {
        WaitUntil::new(self, predicate)
    }
}

impl Default for DiatomicWaker {
    fn default() -> Self {
        Self::new()
    }
}

/// A future that can be `await`ed until a predicate is satisfied.
#[derive(Debug)]
pub struct WaitUntil<'a, P, T>
where
    P: FnMut() -> Option<T>,
{
    predicate: P,
    wake: &'a DiatomicWaker,
}

impl<'a, P, T> WaitUntil<'a, P, T>
where
    P: FnMut() -> Option<T>,
{
    /// Creates a future associated to the specified wake that can be `await`ed
    /// until the specified predicate is satisfied.
    fn new(wake: &'a DiatomicWaker, predicate: P) -> Self {
        Self { predicate, wake }
    }
}

impl<P: FnMut() -> Option<T>, T> Unpin for WaitUntil<'_, P, T> {}

impl<'a, P, T> Future for WaitUntil<'a, P, T>
where
    P: FnMut() -> Option<T>,
{
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        // Safety: the safety of this method is contingent on the safety of the
        // `register` and `unregister` methods. Since a `WaitUntil` future can
        // only be created from the unsafe `wait_until` method, however, the
        // user must uphold the contract that `register`, `unregister` and
        // `wait_until` cannot be used concurrently from multiple threads.
        unsafe {
            if let Some(value) = (self.predicate)() {
                return Poll::Ready(value);
            }
            self.wake.register(cx.waker());

            if let Some(value) = (self.predicate)() {
                self.wake.unregister();
                return Poll::Ready(value);
            }
        }

        Poll::Pending
    }
}
