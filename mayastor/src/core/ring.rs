use spdk_sys::{
    spdk_ring,
    spdk_ring_create,
    SPDK_ENV_SOCKET_ID_ANY,
    SPDK_RING_TYPE_MP_SC,
};

#[derive(Debug)]
pub struct Ring(pub(crate) *mut spdk_ring);

impl Ring {
    pub fn new(count: u64) -> Self {
        trace!("allocating ring...");
        let ring = unsafe {
            spdk_ring_create(
                SPDK_RING_TYPE_MP_SC,
                count,
                SPDK_ENV_SOCKET_ID_ANY,
            )
        };

        if ring.is_null() {
            error!("Failed to allocate MP_SC ring for main threads");
            std::process::exit(-1);
        } else {
            trace!("ring allocated at {:p}", ring);
            Self(ring)
        }
    }
}
