// Remaining users: headers::write_setup_header (the Q5-only convenience used
// by tests/headers.rs and tests/ogg_headers.rs). The encoder itself builds
// every setup header with setup_gen::build_setup. Delete this module once
// write_setup_header is gone.
pub(crate) static Q5_SETUP_MONO44: &[u8] = include_bytes!("setup_blob_mono44.bin");
pub(crate) static Q5_SETUP_MONO48: &[u8] = include_bytes!("setup_blob_mono48.bin");
pub(crate) static Q5_SETUP_STEREO44: &[u8] = include_bytes!("setup_blob_stereo44.bin");
pub(crate) static Q5_SETUP_STEREO48: &[u8] = include_bytes!("setup_blob_stereo48.bin");
