use std::pin::Pin;

use futures::Stream;

mod netns;

pub use netns::{NetnsService, NetnsServiceServer};

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error(transparent)]
    Netns(#[from] ::netns::Error),
}

type ResponseStream<T> = Pin<Box<dyn Stream<Item = tonic::Result<T>> + Send>>;
