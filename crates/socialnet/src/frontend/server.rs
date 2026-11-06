use std::env;
use std::net::SocketAddr;
use std::sync::Arc;

use tonic::async_trait;
use tonic::transport::Channel;
use tonic::transport::Server;
use tonic::{Request, Response, Status};

use crate::compose_post::compose_post_service_client::ComposePostServiceClient;
use crate::compose_post::{ComposePostRequest, ComposePostResponse};
use crate::config::HttpSlosMs;
use crate::frontend::frontend_service_server::{FrontendService, FrontendServiceServer};
use crate::frontend::http;
use crate::frontend::{PingRequest, PingResponse};
use crate::home_timeline::home_timeline_service_client::HomeTimelineServiceClient;
use crate::home_timeline::{ReadHomeTimelineRequest, ReadHomeTimelineResponse};
use crate::register_user::register_user_service_client::RegisterUserServiceClient;
use crate::register_user::{RegisterUserRequest, RegisterUserResponse};
use crate::social_graph::social_graph_service_client::SocialGraphServiceClient;
use crate::social_graph::{
    FollowRequest, FollowResponse, FollowWithUsernameRequest, FollowWithUsernameResponse,
    GetFolloweesRequest, GetFolloweesResponse, GetFollowersRequest, GetFollowersResponse,
    UnfollowRequest, UnfollowResponse, UnfollowWithUsernameRequest, UnfollowWithUsernameResponse,
};
use crate::user::user_service_client::UserServiceClient;
use crate::user::{
    LoginRequest, LoginResponse, RegisterUserWithIdRequest, RegisterUserWithIdResponse,
};
use crate::user_timeline::user_timeline_service_client::UserTimelineServiceClient;
use crate::user_timeline::{ReadUserTimelineRequest, ReadUserTimelineResponse};

#[derive(Clone, Debug)]
pub struct Args {
    pub listen_addr: String,
    pub compose_post_ip: String,
    pub compose_post_port: u16,
    pub compose_post_replicas: u8,
    pub register_user_ip: String,
    pub register_user_port: u16,
    pub register_user_replicas: u8,
    /// Listen address of the HTTP/JSON API (`FRONTEND_HTTP_LISTEN_ADDR`).
    pub http_listen_addr: String,
    /// Per-route SLOs (`config/socialnet.json`, see `crate::config`).
    pub slo_ms: HttpSlosMs,
    /// HS256 secret of the login tokens (`JWT_SECRET`, the user service's);
    /// the cookie-authenticated routes answer 501 without it.
    pub jwt_secret: Option<String>,
    pub user_service: Downstream,
    pub social_graph: Downstream,
    pub user_timeline: Downstream,
    pub home_timeline: Downstream,
}

/// Where a downstream service lives (`<NAME>_IP`, `_PORT`, `_REPLICAS`).
#[derive(Clone, Debug)]
pub struct Downstream {
    pub ip: String,
    pub port: u16,
    pub replicas: u8,
}

impl Downstream {
    fn from_env(prefix: &str, default_ip: &str) -> Self {
        let var = |suffix: &str| env::var(format!("{prefix}_{suffix}")).ok();
        Self {
            ip: var("IP").unwrap_or_else(|| default_ip.to_string()),
            port: var("PORT")
                .unwrap_or_else(|| "8080".to_string())
                .parse()
                .unwrap_or_else(|_| panic!("{prefix}_PORT must be a valid number")),
            replicas: var("REPLICAS")
                .unwrap_or_else(|| "1".to_string())
                .parse()
                .unwrap_or_else(|_| panic!("{prefix}_REPLICAS must be a valid number")),
        }
    }
}

impl Args {
    pub fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            listen_addr: env::var("FRONTEND_LISTEN_ADDR")
                .unwrap_or_else(|_| "0.0.0.0:8080".to_string()),
            compose_post_ip: env::var("COMPOSE_POST_SERVICE_IP")
                .unwrap_or_else(|_| "compose-post-service".to_string()),
            compose_post_port: env::var("COMPOSE_POST_SERVICE_PORT")
                .unwrap_or_else(|_| "8080".to_string())
                .parse()
                .expect("COMPOSE_POST_SERVICE_PORT must be a valid number"),
            compose_post_replicas: env::var("COMPOSE_POST_SERVICE_REPLICAS")
                .unwrap_or_else(|_| "1".to_string())
                .parse()
                .expect("COMPOSE_POST_SERVICE_REPLICAS must be a valid number"),
            register_user_ip: env::var("REGISTER_USER_SERVICE_IP")
                .unwrap_or_else(|_| "register-user-service".to_string()),
            register_user_port: env::var("REGISTER_USER_SERVICE_PORT")
                .unwrap_or_else(|_| "8080".to_string())
                .parse()
                .expect("REGISTER_USER_SERVICE_PORT must be a valid number"),
            register_user_replicas: env::var("REGISTER_USER_SERVICE_REPLICAS")
                .unwrap_or_else(|_| "1".to_string())
                .parse()
                .expect("REGISTER_USER_SERVICE_REPLICAS must be a valid number"),
            http_listen_addr: env::var("FRONTEND_HTTP_LISTEN_ADDR")
                .unwrap_or_else(|_| "0.0.0.0:5000".to_string()),
            slo_ms: crate::config::load_http_slos()?,
            jwt_secret: env::var("JWT_SECRET").ok().filter(|s| !s.is_empty()),
            user_service: Downstream::from_env("USER_SERVICE", "user-service"),
            social_graph: Downstream::from_env("SOCIAL_GRAPH_SERVICE", "social-graph-service"),
            user_timeline: Downstream::from_env("USER_TIMELINE_SERVICE", "user-timeline-service"),
            home_timeline: Downstream::from_env("HOME_TIMELINE_SERVICE", "home-timeline-service"),
        })
    }
}

#[derive(Clone)]
pub struct FrontendServiceImpl {
    compose_post: Channel,
    register_user: Channel,
    user_service: Channel,
    social_graph: Channel,
    user_timeline: Channel,
    home_timeline: Channel,
}

impl FrontendServiceImpl {
    pub async fn new(args: &Args) -> Result<Self, Box<dyn std::error::Error>> {
        let compose_post = socialnet::connect(
            args.compose_post_ip.clone(),
            args.compose_post_port,
            args.compose_post_replicas,
        )
        .await;
        let register_user = socialnet::connect(
            args.register_user_ip.clone(),
            args.register_user_port,
            args.register_user_replicas,
        )
        .await;
        let connect = |d: &Downstream| crate::connect(d.ip.clone(), d.port, d.replicas);
        let user_service = connect(&args.user_service).await;
        let social_graph = connect(&args.social_graph).await;
        let user_timeline = connect(&args.user_timeline).await;
        let home_timeline = connect(&args.home_timeline).await;
        Ok(Self {
            compose_post,
            register_user,
            user_service,
            social_graph,
            user_timeline,
            home_timeline,
        })
    }
}

#[async_trait]
impl FrontendService for FrontendServiceImpl {
    async fn ping(&self, request: Request<PingRequest>) -> Result<Response<PingResponse>, Status> {
        let msg = request.into_inner().message;
        Ok(Response::new(PingResponse { message: msg }))
    }

    async fn compose_post(
        &self,
        request: Request<ComposePostRequest>,
    ) -> Result<Response<ComposePostResponse>, Status> {
        let mut client = ComposePostServiceClient::new(self.compose_post.clone());
        client.compose_post(request).await
    }

    async fn register_user(
        &self,
        request: Request<RegisterUserRequest>,
    ) -> Result<Response<RegisterUserResponse>, Status> {
        let mut client = RegisterUserServiceClient::new(self.register_user.clone());
        client.register_user(request).await
    }

    async fn register_user_with_id(
        &self,
        request: Request<RegisterUserWithIdRequest>,
    ) -> Result<Response<RegisterUserWithIdResponse>, Status> {
        UserServiceClient::new(self.user_service.clone())
            .register_user_with_id(request)
            .await
    }

    async fn login(
        &self,
        request: Request<LoginRequest>,
    ) -> Result<Response<LoginResponse>, Status> {
        UserServiceClient::new(self.user_service.clone())
            .login(request)
            .await
    }

    async fn follow(
        &self,
        request: Request<FollowRequest>,
    ) -> Result<Response<FollowResponse>, Status> {
        SocialGraphServiceClient::new(self.social_graph.clone())
            .follow(request)
            .await
    }

    async fn unfollow(
        &self,
        request: Request<UnfollowRequest>,
    ) -> Result<Response<UnfollowResponse>, Status> {
        SocialGraphServiceClient::new(self.social_graph.clone())
            .unfollow(request)
            .await
    }

    async fn follow_with_username(
        &self,
        request: Request<FollowWithUsernameRequest>,
    ) -> Result<Response<FollowWithUsernameResponse>, Status> {
        SocialGraphServiceClient::new(self.social_graph.clone())
            .follow_with_username(request)
            .await
    }

    async fn unfollow_with_username(
        &self,
        request: Request<UnfollowWithUsernameRequest>,
    ) -> Result<Response<UnfollowWithUsernameResponse>, Status> {
        SocialGraphServiceClient::new(self.social_graph.clone())
            .unfollow_with_username(request)
            .await
    }

    async fn get_followers(
        &self,
        request: Request<GetFollowersRequest>,
    ) -> Result<Response<GetFollowersResponse>, Status> {
        SocialGraphServiceClient::new(self.social_graph.clone())
            .get_followers(request)
            .await
    }

    async fn get_followees(
        &self,
        request: Request<GetFolloweesRequest>,
    ) -> Result<Response<GetFolloweesResponse>, Status> {
        SocialGraphServiceClient::new(self.social_graph.clone())
            .get_followees(request)
            .await
    }

    async fn read_user_timeline(
        &self,
        request: Request<ReadUserTimelineRequest>,
    ) -> Result<Response<ReadUserTimelineResponse>, Status> {
        UserTimelineServiceClient::new(self.user_timeline.clone())
            .read_user_timeline(request)
            .await
    }

    async fn read_home_timeline(
        &self,
        request: Request<ReadHomeTimelineRequest>,
    ) -> Result<Response<ReadHomeTimelineResponse>, Status> {
        HomeTimelineServiceClient::new(self.home_timeline.clone())
            .read_home_timeline(request)
            .await
    }
}

pub async fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let addr: SocketAddr = args.listen_addr.parse()?;
    let http_addr: SocketAddr = args.http_listen_addr.parse()?;
    let service_impl = FrontendServiceImpl::new(&args).await?;

    // One server instance (one set of policy server state) behind both the
    // gRPC port and the in-process client the HTTP API calls.
    let server = FrontendServiceServer::new(service_impl);
    let http_state = Arc::new(http::HttpFrontend::new(
        http::LocalClient::new(http::InProcess::new(server.clone())),
        args.slo_ms.clone(),
        args.jwt_secret.clone(),
    ));
    tokio::spawn(async move {
        log::info!("HTTP API listening on {}...", http_addr);
        if let Err(e) = http::serve(http_addr, http_state).await {
            log::error!("HTTP server failed: {e}");
            std::process::exit(1);
        }
    });
    log::info!("gRPC server listening on {}...", addr);
    Server::builder().add_service(server).serve(addr).await?;
    Ok(())
}
