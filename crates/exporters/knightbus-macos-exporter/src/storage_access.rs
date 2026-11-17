//! User-session identity for the macOS desktop sharing helper.
use std::ffi::c_int;

#[link(name = "bsm")]
unsafe extern "C" {
    fn getauid(uid: *mut u32) -> c_int;
}
/// Audit identity inherited from the logged-in app through the restricted launcher.
/// System launch daemons have no such user session and cannot prompt on its behalf.
pub fn session_user() -> Option<u32> {
    let mut uid = u32::MAX;
    // SAFETY: getauid writes one audit-user ID to the supplied initialized value.
    if unsafe { getauid(&mut uid) } == 0 && uid != u32::MAX {
        Some(uid)
    } else {
        None
    }
}
