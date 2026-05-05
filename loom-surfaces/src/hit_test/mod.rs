// hit_test — selector → bounding-box-centre resolver shared by the
// pointer-dispatching verbs (Click, Hover, Scroll). See `hit_test.rs` for
// the contract.

pub mod hit_test;

#[cfg(test)]
mod interface_tests;
