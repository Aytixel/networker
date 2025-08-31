use std::sync::Arc;

use futures::StreamExt;
use netns::{Netns, NetnsWatcher, NetnsWatcherStream};
use tonic::Response;

use crate::api::{Error, ResponseStream};

mod proto {
    tonic::include_proto!("kernel.netns");
}

pub use proto::netns_service_server::NetnsServiceServer;

#[derive(Clone)]
pub struct NetnsService {
    netns_watcher: Arc<NetnsWatcher>,
}

impl NetnsService {
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            netns_watcher: NetnsWatcher::new()?,
        })
    }
}

#[tonic::async_trait]
impl proto::netns_service_server::NetnsService for NetnsService {
    type WatchNetnsStream = ResponseStream<proto::NetnsList>;

    async fn watch_netns(
        &self,
        _request: tonic::Request<()>,
    ) -> tonic::Result<tonic::Response<Self::WatchNetnsStream>> {
        let netns_watcher_stream =
            NetnsWatcherStream::new(self.netns_watcher.clone()).map(|list| {
                Ok(proto::NetnsList {
                    list: list
                        .into_iter()
                        .map(|netns| match netns {
                            Netns::Default => proto::Netns { name: None },
                            Netns::Named(name) => proto::Netns { name: Some(name) },
                        })
                        .collect(),
                })
            });

        Ok(Response::new(Box::pin(netns_watcher_stream)))
    }
}
