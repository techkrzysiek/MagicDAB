//! MagicDAB receiver core.
//!
//! Everything needed to turn 2.048 MS/s complex baseband samples of a DAB
//! (transmission mode I) signal into ensemble information, audio, dynamic
//! labels, slideshows and transmitter identification.

pub mod audio;
pub mod charset;
pub mod consts;
pub mod crc;
pub mod dabplus;
pub mod dls;
pub mod ensemble;
pub mod eti;
pub mod etirec;
pub mod fic;
pub mod fig;
pub mod mot;
pub mod msc;
pub mod ofdm;
pub mod packet;
pub mod pad;
pub mod puncture;
pub mod receiver;
pub mod rs;
pub mod spi;
pub mod tables;
pub mod tii;
pub mod viterbi;

pub use num_complex::Complex32;
