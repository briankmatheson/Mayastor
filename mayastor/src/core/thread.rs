use std::ffi::CString;

use snafu::Snafu;

use crate::core::cpu_cores::CpuMask;
use once_cell::sync::OnceCell;
use spdk_sys::{
    spdk_get_thread,
    spdk_set_thread,
    spdk_thread,
    spdk_thread_create,
    spdk_thread_destroy,
    spdk_thread_exit,
    spdk_thread_is_exited,
    spdk_thread_poll,
};

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("Event spawned from a non-spdk thread"))]
    InvalidThread {},
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
/// struct that wraps an SPDK thread. The name thread is chosen poorly and
/// should not be confused with an actual thread. Consider it more to be
/// analogous to a container to which you can submit work and poll it to drive
/// the submitted work to completion.
pub struct Mthread(pub(crate) *mut spdk_thread);

unsafe impl Send for Mthread {}
unsafe impl Sync for Mthread {}

pub static INIT_THREAD: OnceCell<Mthread> = OnceCell::new();

impl Mthread {
    ///
    /// With the given thread as context, execute the closure on that thread.
    ///
    /// Any function can be executed here however, this should typically be used
    /// to execute functions that reference any FFI to SPDK.

    pub fn new(name: String, core: u32) -> Option<Self> {
        let name = CString::new(name).unwrap();
        let t = unsafe {
            let mut mask = CpuMask::new();
            mask.set_cpu(core, true);
            spdk_thread_create(name.as_ptr(), mask.as_ptr())
        };
        Self::from_null_checked(t)
    }
    ///
    /// # Note
    ///
    /// Avoid any blocking calls as it will block the reactor, and avoid
    /// long-running functions in general follow the nodejs event loop
    /// model, and you should be good.
    pub fn with<F: FnOnce()>(self, f: F) -> Self {
        self.enter();
        f();
        self.poll().exit();
        self
    }

    #[inline]
    pub fn poll(self) -> Self {
        let _rc = unsafe { spdk_thread_poll(self.0, 0, 0) };
        self
    }

    #[inline]
    pub fn enter(self) -> Self {
        unsafe { spdk_set_thread(self.0) };
        self
    }

    #[inline]
    pub fn exit(self) -> Self {
        unsafe { spdk_set_thread(std::ptr::null_mut()) };
        self
    }

    pub fn current() -> Option<Mthread> {
        Mthread::from_null_checked(unsafe { spdk_get_thread() })
    }

    pub fn name(&self) -> &str {
        unsafe {
            std::ffi::CStr::from_ptr(&(*self.0).name[0])
                .to_str()
                .unwrap()
        }
    }

    /// destroy the given thread waiting for it to become ready to destroy
    pub fn destroy(self) {
        debug!("destroying thread {}...{:p}", self.name(), self.0);
        unsafe {
            spdk_set_thread(self.0);
            // set that we *want* to exit, but we have not exited yet
            spdk_thread_exit(self.0);

            // now wait until the thread is actually excited the internal
            // state is updated by spdk_thread_poll()
            while !spdk_thread_is_exited(self.0) {
                spdk_thread_poll(self.0, 0, 0);
            }
            spdk_thread_destroy(self.0);
        }

        debug!("thread {:p} destroyed", self.0);
    }

    pub fn inner(self) -> *const spdk_thread {
        self.0
    }

    pub fn inner_mut(self) -> *mut spdk_thread {
        self.0
    }

    pub fn from_null_checked(t: *mut spdk_thread) -> Option<Self> {
        if t.is_null() {
            None
        } else {
            Some(Mthread(t))
        }
    }
}
