mod api;
mod sys;

use std::{net::SocketAddr, str::FromStr};

use tonic::transport;
use tracing_subscriber::FmtSubscriber;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error(transparent)]
    Api(#[from] api::Error),
    #[error(transparent)]
    Transport(#[from] transport::Error),
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing::subscriber::set_global_default(
        FmtSubscriber::builder()
            .with_env_filter(tracing_subscriber::EnvFilter::from_env(
                "NETWORKER_KERNEL_LOG",
            ))
            .finish(),
    )
    .expect("tracing setup failed");

    transport::Server::builder()
        .add_service(api::NetnsServiceServer::new(api::NetnsService::new()?))
        .serve(SocketAddr::from_str("[::1]:50051").unwrap())
        .await?;

    Ok(())
}
