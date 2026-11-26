//! The session picker module: this step's shape. The rows and their
//! filter live in [`sessions`](self::sessions); the panel's own rows
//! follow in this file as the picker generalises.

pub(in crate::tui) mod sessions;

#[cfg(test)]
pub(in crate::tui) mod tests;

pub(in crate::tui) use sessions::*;
