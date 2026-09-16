//! After Magic_Number and Frame_Header, there are some number of blocks. Each frame must have at
//! least one block, but there is no upper limit to the number of blocks.
//!
//! There are a few different kinds of blocks, and implementations for those kinds are
//! in this module.
pub(crate) mod compressed;
pub(crate) mod split;

pub(super) use compressed::*;
