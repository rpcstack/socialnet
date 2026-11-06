//! Stack probe: proves the whole stack is up and answering.
//!
//! 1. Readiness: wait until the frontend answers a ComposePost end to end.
//! 2. Control: `--control` ComposePost requests through the frontend must all
//!    succeed.
//! 3. Every service (the frontend and each `--target name=http://host:port`)
//!    must answer one ordinary request, sent directly to it with a generous
//!    SLO, with success.
//!
//! The probe is deliberately policy-agnostic: the no-op `baseline` and the
//! initial `policy` stack must both pass, and so should any policy that does
//! not reject ordinary traffic. Exit status 0 means every check passed.

use std::time::{Duration, Instant};

use app_utils::root_context::RootContext;
use clap::Parser;
use tonic::transport::Channel;
use tonic::{Request, Status};

use socialnet::*;

mod uniqueid {
    tonic::include_proto!("uniqueidservice");
}

#[derive(Parser, Debug)]
struct Args {
    /// Frontend address, e.g. http://frontend-service:8080.
    #[arg(long)]
    frontend: String,
    /// Extra services to probe directly, as `name=http://host:port`. Known
    /// names: compose_post, register_user, user, unique_id, media, text,
    /// user_mention, url_shorten, post_storage, user_timeline, home_timeline,
    /// social_graph.
    #[arg(long = "target")]
    targets: Vec<String>,
    /// Number of control ComposePost requests (generous SLO) that must succeed.
    #[arg(long, default_value_t = 20)]
    control: usize,
    /// Before probing, retry a ComposePost for up to this many seconds until
    /// the stack answers (readiness wait).
    #[arg(long, default_value_t = 0)]
    wait_secs: u64,
    /// Per-request timeout in milliseconds.
    #[arg(long, default_value_t = 5000)]
    timeout_ms: u64,
}

fn compose_post_request() -> compose_post::ComposePostRequest {
    compose_post::ComposePostRequest {
        req_id: 1,
        username: "user_1".into(),
        user_id: 1,
        text: "probe post".into(),
        ..Default::default()
    }
}

/// A request with a generous (5 s) SLO.
fn live<T>(api: &str, msg: T) -> Request<T> {
    RootContext::new(api, Duration::from_secs(5)).attach(Request::new(msg))
}

async fn channel(addr: &str, timeout: Duration) -> Result<Channel, Box<dyn std::error::Error>> {
    Ok(Channel::from_shared(addr.to_string())?
        .timeout(timeout)
        .connect_timeout(timeout)
        .connect()
        .await?)
}

/// One ordinary request to the named service.
async fn probe(
    name: &str,
    addr: &str,
    timeout: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    let ch = channel(addr, timeout).await?;
    macro_rules! call {
        ($client:ty, $method:ident, $api:expr, $msg:expr) => {{
            let mut c = <$client>::new(ch);
            c.$method(live($api, $msg)).await.map(|_| ())
        }};
    }
    // A name no other probe run uses, so a re-run never hits "already exists".
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let r: Result<(), Status> = match name {
        "frontend" => call!(
            frontend::frontend_service_client::FrontendServiceClient<Channel>,
            compose_post,
            "ComposePost",
            compose_post_request()
        ),
        "compose_post" => call!(
            compose_post::compose_post_service_client::ComposePostServiceClient<Channel>,
            compose_post,
            "ComposePost",
            compose_post_request()
        ),
        "register_user" => call!(
            register_user::register_user_service_client::RegisterUserServiceClient<Channel>,
            register_user,
            "RegisterUser",
            register_user::RegisterUserRequest {
                req_id: 1,
                first_name: "Probe".into(),
                last_name: "User".into(),
                username: format!("probe_{nanos}"),
                password: "probe".into(),
                ..Default::default()
            }
        ),
        "user" => call!(
            user::user_service_client::UserServiceClient<Channel>,
            compose_creator_with_user_id,
            "ComposePost",
            user::ComposeCreatorWithUserIdRequest {
                req_id: 1,
                user_id: 1,
                username: "user_1".into(),
                ..Default::default()
            }
        ),
        "unique_id" => call!(
            uniqueid::unique_id_service_client::UniqueIdServiceClient<Channel>,
            compose_unique_id,
            "ComposePost",
            uniqueid::UniqueIdRequest { id: 1 }
        ),
        "media" => call!(
            media::media_service_client::MediaServiceClient<Channel>,
            compose_media,
            "ComposePost",
            Default::default()
        ),
        "text" => call!(
            text_service::text_service_client::TextServiceClient<Channel>,
            compose_text,
            "ComposePost",
            text_service::TextRequest {
                text: "probe".into()
            }
        ),
        "user_mention" => call!(
            user_mention::user_mention_service_client::UserMentionServiceClient<Channel>,
            compose_user_mentions,
            "ComposePost",
            Default::default()
        ),
        "url_shorten" => call!(
            url_shorten::url_shorten_service_client::UrlShortenServiceClient<Channel>,
            compose_urls,
            "ComposePost",
            Default::default()
        ),
        "post_storage" => call!(
            post_storage::post_storage_service_client::PostStorageServiceClient<Channel>,
            read_posts,
            "ReadUserTimeline",
            Default::default()
        ),
        "user_timeline" => call!(
            user_timeline::user_timeline_service_client::UserTimelineServiceClient<Channel>,
            read_user_timeline,
            "ReadUserTimeline",
            user_timeline::ReadUserTimelineRequest {
                req_id: 1,
                user_id: 1,
                start: 0,
                stop: 10,
                ..Default::default()
            }
        ),
        "home_timeline" => call!(
            home_timeline::home_timeline_service_client::HomeTimelineServiceClient<Channel>,
            read_home_timeline,
            "ReadHomeTimeline",
            home_timeline::ReadHomeTimelineRequest {
                req_id: 1,
                user_id: 1,
                start: 0,
                stop: 10,
                ..Default::default()
            }
        ),
        "social_graph" => call!(
            social_graph::social_graph_service_client::SocialGraphServiceClient<Channel>,
            get_followers,
            "GetFollowers",
            social_graph::GetFollowersRequest {
                req_id: 1,
                user_id: 1,
                ..Default::default()
            }
        ),
        other => return Err(format!("unknown target {other}").into()),
    };
    r.map_err(|s| format!("{:?}: {}", s.code(), s.message()).into())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let timeout = Duration::from_millis(args.timeout_ms);
    let mut failures = Vec::new();

    // 1. Readiness: wait until the frontend answers a ComposePost end to end.
    let deadline = Instant::now() + Duration::from_secs(args.wait_secs);
    let mut fe = loop {
        let attempt = async {
            let ch = channel(&args.frontend, timeout).await?;
            let mut fe = frontend::frontend_service_client::FrontendServiceClient::new(ch);
            fe.compose_post(live("ComposePost", compose_post_request()))
                .await?;
            Ok::<_, Box<dyn std::error::Error>>(fe)
        };
        match attempt.await {
            Ok(fe) => break fe,
            Err(e) if Instant::now() < deadline => {
                println!("waiting for frontend: {e}");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(e) => return Err(format!("frontend not ready: {e}").into()),
        }
    };
    println!("frontend ready");

    // 2. Control: ComposePost requests through the frontend.
    let mut ok = 0;
    for _ in 0..args.control {
        match fe
            .compose_post(live("ComposePost", compose_post_request()))
            .await
        {
            Ok(_) => ok += 1,
            Err(s) => println!("control: error {:?}: {}", s.code(), s.message()),
        }
    }
    println!(
        "control: {ok}/{} ComposePost requests with a 5 s SLO succeeded",
        args.control
    );
    if ok != args.control {
        failures.push(format!("control: only {ok}/{} succeeded", args.control));
    }

    // 3. One ordinary request to every service.
    let mut targets = vec![("frontend".to_string(), args.frontend.clone())];
    for t in &args.targets {
        let (name, addr) = t
            .split_once('=')
            .ok_or_else(|| format!("--target must be name=addr, got {t}"))?;
        targets.push((name.to_string(), addr.to_string()));
    }
    for (name, addr) in &targets {
        match probe(name, addr, timeout).await {
            Ok(()) => println!("service {name:<15} -> ok"),
            Err(e) => {
                println!("service {name:<15} -> {e}");
                failures.push(format!("{name}: {e}"));
            }
        }
    }

    if failures.is_empty() {
        println!("PROBE PASS");
        Ok(())
    } else {
        for f in &failures {
            println!("FAIL: {f}");
        }
        println!("PROBE FAIL");
        std::process::exit(1);
    }
}
