use std::{convert::TryFrom, fmt::Display, sync::Arc};

use nix::errno::Errno;
use serde::{export::Formatter, Serialize};
use snafu::{ResultExt, Snafu};

use spdk_sys::{spdk_bdev_module_release_bdev, spdk_io_channel};

use crate::{
    bdev::NexusErrStore,
    core::{Bdev, BdevHandle, CoreError, Descriptor, DmaBuf},
    nexus_uri::{bdev_destroy, BdevCreateDestroy},
    subsys::Config,
};

#[derive(Debug, Snafu)]
pub enum ChildError {
    #[snafu(display("Child is not closed"))]
    ChildNotClosed {},
    #[snafu(display(
        "Child is smaller than parent {} vs {}",
        child_size,
        parent_size
    ))]
    ChildTooSmall { child_size: u64, parent_size: u64 },
    #[snafu(display("Open child"))]
    OpenChild { source: CoreError },
    #[snafu(display("Claim child"))]
    ClaimChild { source: Errno },
    #[snafu(display("Child is closed"))]
    ChildClosed {},
    #[snafu(display("Invalid state of child"))]
    ChildInvalid {},
    #[snafu(display("Opening child bdev without bdev pointer"))]
    OpenWithoutBdev {},
    #[snafu(display("Failed to create a BdevHandle for child"))]
    HandleCreate { source: CoreError },
}

#[derive(Debug, Snafu)]
pub enum ChildIoError {
    #[snafu(display("Error writing to {}: {}", name, source))]
    WriteError { source: CoreError, name: String },
    #[snafu(display("Error reading from {}: {}", name, source))]
    ReadError { source: CoreError, name: String },
    #[snafu(display("Invalid descriptor for child bdev {}", name))]
    InvalidDescriptor { name: String },
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
pub(crate) enum ChildState {
    /// child has not been opened, but we are in the process of opening it
    Init,
    /// cannot add this bdev to the parent as its incompatible property wise
    ConfigInvalid,
    /// the child is open for RW
    Open,
    /// The child has been closed by its parent
    Closed,
    /// a non-fatal have occurred on this child
    Faulted,
}

impl ToString for ChildState {
    fn to_string(&self) -> String {
        match *self {
            ChildState::Init => "init",
            ChildState::ConfigInvalid => "configInvalid",
            ChildState::Open => "open",
            ChildState::Faulted => "faulted",
            ChildState::Closed => "closed",
        }
        .parse()
        .unwrap()
    }
}

#[derive(Debug, Serialize)]
pub struct NexusChild {
    /// name of the parent this child belongs too
    pub(crate) parent: String,
    /// Name of the child is the URI used to create it.
    /// Note that bdev name can differ from it!
    pub(crate) name: String,
    #[serde(skip_serializing)]
    /// the bdev wrapped in Bdev
    pub(crate) bdev: Option<Bdev>,
    #[serde(skip_serializing)]
    /// channel on which we submit the IO
    pub(crate) ch: *mut spdk_io_channel,
    #[serde(skip_serializing)]
    pub(crate) desc: Option<Arc<Descriptor>>,
    /// current state of the child
    pub(crate) state: ChildState,
    pub(crate) repairing: bool,
    /// descriptor obtained after opening a device
    #[serde(skip_serializing)]
    pub(crate) bdev_handle: Option<BdevHandle>,
    /// record of most-recent IO errors
    #[serde(skip_serializing)]
    pub(crate) err_store: Option<NexusErrStore>,
}

impl Display for NexusChild {
    fn fmt(&self, f: &mut Formatter) -> Result<(), std::fmt::Error> {
        if self.bdev.is_some() {
            let bdev = self.bdev.as_ref().unwrap();
            writeln!(
                f,
                "{}: {:?}, blk_cnt: {}, blk_size: {}",
                self.name,
                self.state,
                bdev.num_blocks(),
                bdev.block_len(),
            )
        } else {
            writeln!(f, "{}: state {:?}", self.name, self.state)
        }
    }
}

impl NexusChild {
    /// Open the child in RW mode and claim the device to be ours. If the child
    /// is already opened by someone else (i.e one of the targets) it will
    /// error out.
    ///
    /// only devices in the closed or Init state can be opened.
    pub(crate) fn open(
        &mut self,
        parent_size: u64,
    ) -> Result<String, ChildError> {
        trace!("{}: Opening child device {}", self.parent, self.name);

        if self.state != ChildState::Closed && self.state != ChildState::Init {
            return Err(ChildError::ChildNotClosed {});
        }

        if self.bdev.is_none() {
            return Err(ChildError::OpenWithoutBdev {});
        }

        let bdev = self.bdev.as_ref().unwrap();

        let child_size = bdev.size_in_bytes();
        if parent_size > child_size {
            error!(
                "{}: child to small parent size: {} child size: {}",
                self.name, parent_size, child_size
            );
            self.state = ChildState::ConfigInvalid;
            return Err(ChildError::ChildTooSmall {
                parent_size,
                child_size,
            });
        }

        self.desc = Some(Arc::new(
            Bdev::open_by_name(&bdev.name(), true).context(OpenChild {})?,
        ));

        self.bdev_handle = Some(
            BdevHandle::try_from(self.desc.as_ref().unwrap().clone()).unwrap(),
        );

        let cfg = Config::by_ref();
        if cfg.err_store_opts.enable_err_store {
            self.err_store =
                Some(NexusErrStore::new(cfg.err_store_opts.err_store_size));
        };

        self.state = ChildState::Open;

        debug!("{}: child {} opened successfully", self.parent, self.name);

        Ok(self.name.clone())
    }

    /// return a descriptor to this child
    pub fn get_descriptor(&self) -> Result<Arc<Descriptor>, CoreError> {
        if let Some(ref d) = self.desc {
            Ok(d.clone())
        } else {
            Err(CoreError::InvalidDescriptor {
                name: self.name.clone(),
            })
        }
    }

    /// close the bdev -- we have no means of determining if this succeeds
    pub(crate) fn close(&mut self) -> ChildState {
        trace!("{}: Closing child {}", self.parent, self.name);

        if let Some(bdev) = self.bdev.as_ref() {
            unsafe {
                if !(*bdev.as_ptr()).internal.claim_module.is_null() {
                    spdk_bdev_module_release_bdev(bdev.as_ptr());
                }
            }
        }

        // just to be explicit
        let hdl = self.bdev_handle.take();
        let desc = self.desc.take();
        drop(hdl);
        drop(desc);

        // we leave the child structure around for when we want reopen it
        self.state = ChildState::Closed;
        self.state
    }

    /// create a new nexus child
    pub fn new(name: String, parent: String, bdev: Option<Bdev>) -> Self {
        NexusChild {
            name,
            bdev,
            parent,
            desc: None,
            ch: std::ptr::null_mut(),
            state: ChildState::Init,
            bdev_handle: None,
            repairing: false,
            err_store: None,
        }
    }

    /// destroy the child bdev
    pub(crate) async fn destroy(&mut self) -> Result<(), BdevCreateDestroy> {
        trace!("destroying child {:?}", self);
        assert_eq!(self.state, ChildState::Closed);
        if let Some(_bdev) = &self.bdev {
            bdev_destroy(&self.name).await
        } else {
            warn!("Destroy child without bdev");
            Ok(())
        }
    }

    /// returns if a child can be written to
    pub fn can_rw(&self) -> bool {
        self.state == ChildState::Open || self.state == ChildState::Faulted
    }

    /// return references to child's bdev and descriptor
    /// both must be present - otherwise it is considered an error
    pub fn get_dev(&self) -> Result<(&Bdev, &BdevHandle), ChildError> {
        if !self.can_rw() {
            info!("{}: Closed child: {}", self.parent, self.name);
            return Err(ChildError::ChildClosed {});
        }

        if let Some(bdev) = &self.bdev {
            if let Some(desc) = &self.bdev_handle {
                return Ok((bdev, desc));
            }
        }

        Err(ChildError::ChildInvalid {})
    }

    /// write the contents of the buffer to this child
    pub async fn write_at(
        &self,
        offset: u64,
        buf: &DmaBuf,
    ) -> Result<usize, ChildIoError> {
        match self.bdev_handle.as_ref() {
            Some(desc) => {
                Ok(desc.write_at(offset, buf).await.context(WriteError {
                    name: self.name.clone(),
                })?)
            }
            None => Err(ChildIoError::InvalidDescriptor {
                name: self.name.clone(),
            }),
        }
    }

    /// read from this child device into the given buffer
    pub async fn read_at(
        &self,
        offset: u64,
        buf: &mut DmaBuf,
    ) -> Result<usize, ChildIoError> {
        match self.bdev_handle.as_ref() {
            Some(desc) => {
                Ok(desc.read_at(offset, buf).await.context(ReadError {
                    name: self.name.clone(),
                })?)
            }
            None => Err(ChildIoError::InvalidDescriptor {
                name: self.name.clone(),
            }),
        }
    }
}
