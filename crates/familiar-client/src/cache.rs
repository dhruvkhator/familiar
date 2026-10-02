//! Stale-while-revalidate store and in-flight GET de-duplication, keyed by path+query.

use crate::error::ApiError;
use futures_util::future::{BoxFuture, Shared};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;

pub(crate) type Flight = Shared<BoxFuture<'static, Result<Value, ApiError>>>;

#[derive(Default)]
pub(crate) struct Cache {
    /// Last good response per key; survives mutations (shown stale while revalidating).
    pub values: Mutex<HashMap<String, Value>>,
    /// Identical concurrent GETs share one request. Cleared by any mutation or live notice.
    pub inflight: Mutex<HashMap<String, Flight>>,
}

impl Cache {
    pub fn clear_all(&self) {
        self.values.lock().unwrap().clear();
        self.inflight.lock().unwrap().clear();
    }
    pub fn clear_inflight(&self) {
        self.inflight.lock().unwrap().clear();
    }
    pub fn invalidate_prefix(&self, prefix: &str) {
        self.values.lock().unwrap().retain(|k, _| !k.starts_with(prefix));
        self.inflight.lock().unwrap().retain(|k, _| !k.starts_with(prefix));
    }
}
