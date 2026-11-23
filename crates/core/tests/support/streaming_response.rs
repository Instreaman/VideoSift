use std::convert::Infallible;
use std::pin::Pin;
use std::task::{Context, Poll};

/// An unknown-length response that sends the given number of bytes and then stalls.
pub fn streaming_response(bytes: usize) -> reqwest::Response {
    http::Response::new(reqwest::Body::wrap(StallingBody(bytes))).into()
}

struct StallingBody(usize);

impl http_body::Body for StallingBody {
    type Data = &'static [u8];
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        const CHUNK: &[u8] = &[0; 8192];
        if self.0 == 0 {
            return Poll::Pending;
        }
        let count = self.0.min(CHUNK.len());
        self.0 -= count;
        Poll::Ready(Some(Ok(http_body::Frame::data(&CHUNK[..count]))))
    }
}
