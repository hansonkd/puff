pub use reqwest::Client;
use reqwest::{RequestBuilder, Response};
use crate::runtime::{yield_to_future, yield_to_future_io};
use crate::types::{Bytes, Puff, Text};
use crate::errors::Result;


pub trait PuffRequestBuilder {
    fn puff_response(self) -> Result<Response>;
}

impl PuffRequestBuilder for RequestBuilder {
    fn puff_response(self) -> Result<Response> {
        Ok(yield_to_future_io(self.send())??)
    }
}


pub trait PuffClientResponse {
    fn puff_text(self) -> Result<Text>;
    fn puff_bytes(self) -> Result<Bytes>;
    fn puff_chunk(&mut self) -> Result<Option<Bytes>>;
}

impl PuffClientResponse for Response {
    fn puff_text(self) -> Result<Text> {
        Ok(yield_to_future(self.text())?.into())
    }

    fn puff_bytes(self) -> Result<Bytes> {
        Ok(Bytes::from(yield_to_future(self.bytes())?))
    }

    fn puff_chunk(&mut self) -> Result<Option<Bytes>> {
        Ok(yield_to_future(self.chunk())?.map(|v| Bytes::from(v)))
    }
}


impl Puff for Client {}