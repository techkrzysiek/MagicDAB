//! Audio codecs for MagicDAB.
//!
//! * AAC (DAB+): libfaad2 via FFI (GPL-2.0-or-later)
//! * MPEG-1/2 Layer II (DAB): minimp3 (CC0), compiled from third_party
//! * Opus encoding for web streaming: libopus (BSD)
//! * Polyphase resampler to 48 kHz

pub mod aac;
pub mod mp2;
pub mod opus;
pub mod resample;

pub use aac::AacDecoder;
pub use mp2::Mp2Decoder;
pub use opus::OpusEncoder;
pub use resample::Resampler;
