//! Parsing pipeline: drives plugins per line to produce `ParsedLine`s, and
//! builds a line-offset index for fast seek.

pub mod index;
pub mod parser;
