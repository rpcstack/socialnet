extern crate self as socialnet;

pub mod media {
    tonic::include_proto!("media");
    pub mod server;
}

pub mod url_shorten {
    tonic::include_proto!("url_shorten");
    pub mod db;
    pub mod server;
}

pub mod user_mention {
    tonic::include_proto!("usermention");

    pub mod db;
    pub mod server;
}

pub use user_mention as usermention;

pub mod text_service {
    tonic::include_proto!("textservice");

    pub mod server;
}

pub mod user {
    tonic::include_proto!("user");

    pub mod server;
}

pub mod user_timeline {
    tonic::include_proto!("user_timeline");

    pub mod server;
}

pub mod home_timeline {
    tonic::include_proto!("home_timeline");
}

pub mod post_storage {
    tonic::include_proto!("post_storage");

    pub use crate::media;
    pub use crate::url_shorten;
    pub use crate::user;
    pub use crate::user_mention as usermention;
    pub use crate::user_timeline;

    pub mod server;
}

pub mod compose_post {
    tonic::include_proto!("compose_post");

    pub mod server;
}

pub mod register_user {
    tonic::include_proto!("register_user");

    pub mod server;
}

pub mod social_graph {
    tonic::include_proto!("social_graph");
}

pub mod config;

pub mod frontend {
    tonic::include_proto!("frontend");

    pub mod http;
    pub mod server;
}

use std::time::Duration;

use tonic::transport::{Channel, Endpoint};

/// Connect to a downstream socialnet service.
///
/// Replicas are addressed as `ip-1 .. ip-N` (one hostname per replica: the
/// Compose container names, or Kubernetes Services named `<service>-<n>`), as
/// in the original benchmark. With one replica the connection is retried until
/// the service is up; with several the client balances over all of them.
pub async fn connect(ip: String, port: u16, replicas: u8) -> Channel {
    assert!(replicas > 0, "at least one replica required");
    let endpoints =
        (1..=replicas).map(|i| Endpoint::from_shared(format!("http://{ip}-{i}:{port}")).unwrap());
    if replicas == 1 {
        let endpoint = endpoints.into_iter().next().unwrap();
        loop {
            match endpoint.connect().await {
                Ok(channel) => return channel,
                Err(error) => {
                    log::warn!("failed to connect to {}: {error}; retrying", endpoint.uri());
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
            }
        }
    } else {
        Channel::balance_list(endpoints)
    }
}
