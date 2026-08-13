//! OCI image handling: registry pull, layer unpacking, local store.

pub mod load;
pub mod reference;
pub mod registry;
pub mod store;
pub mod unpack;

pub use reference::ImageReference;
pub use registry::{PullEvent, RegistryClient};
pub use store::{ImageStore, StoredImage};

use mvm_common::{Error, Result};

pub(crate) fn img_err(msg: impl Into<String>) -> Error {
    Error::Image(msg.into())
}

pub(crate) type ImgResult<T> = Result<T>;
