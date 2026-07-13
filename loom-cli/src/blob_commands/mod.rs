pub mod blob_commands;
pub use blob_commands::{get, BlobCmd, BlobGetArgs};

#[cfg(test)]
mod interface_tests;
