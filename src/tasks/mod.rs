use lazy_static::lazy_static;

pub mod dispatcher;
pub mod runner;
pub mod task;

use crate::errors::Error;
use dispatcher::Dispatcher;
use std::sync::{Arc, Mutex};

// lazy_static! {
//     pub static ref GLOBAL_DISPATCHER: Dispatcher = Dispatcher::default();
// }

tokio::task_local! {
    pub static DISPATCHER: Arc<Dispatcher>;
}
