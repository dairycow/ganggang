//! TUI view state and drawing: the transcript model
//! ([`transcript`](super::transcript)) rendered into a composer
//! ([`composer`](super::composer)) and a footer ([`footer`](super::footer)),
//! with bottom-anchored scrolling and a suggestion popup.
//!
//! Three pieces, one per concern: [`State`] holds what the loop and the
//! commands change ([`state`]), [`render`] paints one frame from it
//! ([`draw`]), and [`welcome`] is what an empty session says instead of
//! nothing. Colour appears in none of them —
//! [`theme`](super::theme) owns it, the transcript model owns what a line
//! is, and [`layout`](super::layout) owns where everything goes.

mod draw;
mod state;
pub mod welcome;

pub use draw::render;
pub use state::{Cancel, State};
