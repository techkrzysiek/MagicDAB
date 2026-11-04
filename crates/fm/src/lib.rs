//! FM reception: an FM-DX-Tuner (e.g. the TEF668x headless USB tuner) on a
//! serial port, the tuner's audio and RDS decoding with librdsparser.

pub mod capture;
pub mod rds;
pub mod tuner;
