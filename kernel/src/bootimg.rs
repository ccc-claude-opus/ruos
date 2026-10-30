//! What the kernel image carries: `init` (the first program) and `boot`
//! (a pack of the object store, handed to init as a memory object).
include!(concat!(env!("OUT_DIR"), "/boot_images.rs"));

pub fn find(name: &str) -> Option<&'static [u8]> {
    IMAGES.iter().find(|(n, _)| *n == name).map(|(_, b)| *b)
}
