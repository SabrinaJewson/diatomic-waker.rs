use core::mem::ManuallyDrop;
use core::ptr;
use core::sync::atomic::Ordering;
use core::task::RawWaker;
use core::task::RawWakerVTable;
use core::task::Waker;

#[derive(Debug)]
pub(crate) struct DiatomicWaker {
    // The lowest bit (which we can use since `RawWakerVTable` will have an alignment of at least
    // two) stores whether the waker is "inactive"; if set, wakes on the slot should be ignored
    // (but the slot still holds an owned waker).
    inner: AtomicTwoUsize,
}

// Loom has no 128-bit atomics, so we disable the implementation there.
pub(crate) const ENABLE: bool =
    AtomicTwoUsize::is_always_lock_free() && cfg!(not(all(test, diatomic_waker_loom)));

impl DiatomicWaker {
    #[cfg(not(all(test, diatomic_waker_loom)))]
    pub(crate) const fn new() -> [Self; ENABLE as usize] {
        [const {
            Self {
                inner: AtomicTwoUsize::new(0),
            }
        }; ENABLE as usize]
    }

    pub(crate) fn notify(&self) {
        // Implement a fast path to avoid dropping the waker if it's inactive. This can avoid
        // cloning wakers later on.
        // Ordering: We don't have any data dependencies here, so `Relaxed` is sufficient.
        if self.inner.load(Ordering::Relaxed) & 1 != 0 {
            return;
        }

        // Ordering:
        // - Acquire is necessary since we access the `Waker`'s methods after this, thus need to
        //   acquire the current shared state of the waker.
        // - Release is not necessary, since we're just storing a constant (zero) without dependent
        //   data.
        let waker = self.inner.swap(0, Ordering::Acquire);

        // Wake the waker if there is an active one.
        if let Some((waker, false)) = unsafe { decode_waker(waker) } {
            waker.wake();
        }
    }

    pub(crate) fn register(&self, waker: &Waker) {
        // Implement a fast path that avoids cloning the waker.
        // Ordering: Acquire is unnecessary, since we don't do anything with the `Waker` that we
        // load.
        let initial = self.inner.load(Ordering::Relaxed);
        if initial & (!1) == encode_waker(waker) {
            // If the waker was active, we're good. If the waker was inactive, we try to make it
            // active; if this fails, then a concurrent call to `notify` has dropped the stored
            // clone of the waker, so we should clone our waker in as usual.
            // Ordering:
            // - Acquire is unnecessary, since if the value changed, it must have changed to zero,
            //   and in this case there is nothing to acquire.
            // - On sucess, Release ensures that any calls to `wake` happen-after this function
            //   call. This shouldn't be necessary for soundness, but is probably useful for
            //   sanity.
            if initial & 1 == 0
                || self
                    .inner
                    .compare_exchange(
                        initial,
                        initial & (!1),
                        Ordering::Release,
                        Ordering::Relaxed,
                    )
                    .is_ok()
            {
                return;
            }
        }

        // Clone and swap in our new waker, then the drop the old one.
        let waker = encode_waker(&ManuallyDrop::new(waker.clone()));

        // Ordering:
        // - Acquire is necessary, since we drop the old waker, and thus need to acquire its current
        //   shared state.
        // - Release is necessary, as other threads need to be able to acquire the state changes
        //   produced by the above `.clone()`.
        let old_waker = self.inner.swap(waker, Ordering::AcqRel);
        drop(unsafe { decode_waker(old_waker) });
    }

    pub(crate) fn unregister(&self) {
        // Just set the inactive bit. If there is no waker that's also okay.
        self.inner.fetch_or(1, Ordering::Relaxed);
    }
}

#[cfg(target_pointer_width = "32")]
type TwoUsize = u64;

#[cfg(target_pointer_width = "32")]
type AtomicTwoUsize = portable_atomic::AtomicU64;

#[cfg(target_pointer_width = "64")]
type TwoUsize = u128;

#[cfg(target_pointer_width = "64")]
type AtomicTwoUsize = portable_atomic::AtomicU128;

/// "Encode" a `Waker` into its two-usize-based representation. The waker will be active.
fn encode_waker(waker: &Waker) -> TwoUsize {
    // Needed since we use the least significant bit to store the "active" state.
    const { assert!(2 <= align_of::<RawWakerVTable>()) };

    let data = waker.data().expose_provenance();
    let vtable = ptr::from_ref(waker.vtable()).expose_provenance();

    (data as TwoUsize) << usize::BITS | (vtable as TwoUsize)
}

/// "Decode" a `Waker` from its two-usize-based representation, or `None` if the given value was
/// zero. Returns a (waker, inactive) tuple.
///
/// # Safety
///
/// The value (excluding its last bit) must have previously been obtained from `encode_waker`, or be
/// zero.
unsafe fn decode_waker(val: TwoUsize) -> Option<(Waker, bool)> {
    let inactive = val & 1 != 0;
    let val = val & (!1);

    if val == 0 {
        return None;
    }

    let data = ptr::with_exposed_provenance::<()>((val >> usize::BITS) as usize);

    // Safety: Ensured by the caller
    let vtable = unsafe { &*ptr::with_exposed_provenance::<RawWakerVTable>(val as usize) };

    let raw_waker = RawWaker::new(data, vtable);

    // Safety: Ensured by the caller
    Some((unsafe { Waker::from_raw(raw_waker) }, inactive))
}
