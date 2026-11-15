//! Checked, allocation-free messages shared by the Windows importer and driver.
//! No USB/IP parsing or native handles cross this boundary.
#![no_std]
pub mod descriptors;
pub mod wire;
