//! Primitives for communicating between Tasks
//!
//! There are two types of channels: oneshot and mpsc. Channels are a non-blocking way to communicate
//! between Tasks.
//!
//! # MPSC
//!
//! `mpsc` channels can be used when you want to send multiple values to a receiver. `mpsc` channels are
//! unbounded so care should be used that too many messages don't flood a channel.
//!
//! # Oneshot
//!
//! `oneshot` channels can be used when you want to send exactly one value to a receiver.
//!
pub mod mpsc;
pub mod oneshot;
