//! Policy-liveness probe.
//!
//! Proves that real RPCs go through Masa's hooks in every socialnet service,
//! with a check that cannot pass in baseline mode:
//!
//! 1. Readiness and control: `--control` ComposePost requests through the
//!    frontend with a generous SLO must all succeed (in both modes), so the
//!    probe is not just rejecting everything.
//! 2. Expired deadline: for each service, send one request whose Masa context
//!    (the header written by `RootContext::attach`) has an end-to-end deadline
//!    already in the past. Masa's `abort_slo` deadline guard runs in the server
//!    hooks before the handler is polled and answers `DEADLINE_EXCEEDED
//!    "/EarlyReturn?src=<package.Service>::<Method>"`. With no-op hooks the
//!    header is ignored, the handler runs, and that string cannot be produced:
//!    it only exists in Masa's policy modules.
//!
//! `--expect masa` requires an `/EarlyReturn` from every target; `--expect
//! baseline` requires none; `--expect none` only waits for readiness. Exit
//! status 0 means the expectation held.

use std::time::{Duration, Instant};

use clap::Parser;
use masa::RootContext;
use tonic::transport::Channel;
use tonic::{Request, Status};

use socialnet::*;

mod uniqueid {
    tonic::include_proto!("uniqueidservice");
}
mod social_graph {
    tonic::include_proto!("social_graph");
}

#[derive(Parser, Debug)]
struct Args {
    /// `baseline`, `masa`, or `none` (readiness only).
    #[arg(long)]
    expect: String,
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

#[derive(Debug)]
enum Outcome {
    Ok,
    EarlyReturn(String),
    Error(#[allow(dead_code)] String),
}

impl Outcome {
    fn of<T>(r: Result<tonic::Response<T>, Status>) -> Self {
        match r {
            Ok(_) => Outcome::Ok,
            Err(s) if s.message().starts_with("/EarlyReturn") => {
                Outcome::EarlyReturn(s.message().to_string())
            }
            Err(s) => Outcome::Error(format!("{:?}: {}", s.code(), s.message())),
        }
    }
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

/// A Masa root context whose end-to-end deadline has already passed.
async fn expired<T>(api: &str, msg: T) -> Request<T> {
    let ctx = RootContext::from(masa::create_context(api, Duration::from_micros(1)));
    tokio::time::sleep(Duration::from_millis(2)).await;
    ctx.attach(Request::new(msg))
}

async fn channel(addr: &str, timeout: Duration) -> Result<Channel, Box<dyn std::error::Error>> {
    Ok(Channel::from_shared(addr.to_string())?
        .timeout(timeout)
        .connect_timeout(timeout)
        .connect()
        .await?)
}

/// gRPC service name each probe target is expected to report in `src=`.
fn service_of(name: &str) -> Option<&'static str> {
    Some(match name {
        "frontend" => "frontend.FrontendService",
        "compose_post" => "compose_post.ComposePostService",
        "register_user" => "register_user.RegisterUserService",
        "user" => "user.UserService",
        "unique_id" => "uniqueidservice.UniqueIdService",
        "media" => "media.MediaService",
        "text" => "textservice.TextService",
        "user_mention" => "usermention.UserMentionService",
        "url_shorten" => "url_shorten.UrlShortenService",
        "post_storage" => "post_storage.PostStorageService",
        "user_timeline" => "user_timeline.UserTimelineService",
        "home_timeline" => "home_timeline.HomeTimelineService",
        "social_graph" => "social_graph.SocialGraphService",
        _ => return None,
    })
}

async fn probe(
    name: &str,
    addr: &str,
    timeout: Duration,
) -> Result<Outcome, Box<dyn std::error::Error>> {
    let ch = channel(addr, timeout).await?;
    macro_rules! call {
        ($client:ty, $method:ident, $api:expr) => {{
            let mut c = <$client>::new(ch);
            Outcome::of(c.$method(expired($api, Default::default()).await).await)
        }};
    }
    let outcome = match name {
        "frontend" => {
            let mut c = frontend::frontend_service_client::FrontendServiceClient::new(ch);
            Outcome::of(
                c.compose_post(expired("ComposePost", compose_post_request()).await)
                    .await,
            )
        }
        "compose_post" => call!(
            compose_post::compose_post_service_client::ComposePostServiceClient<Channel>,
            compose_post,
            "ComposePost"
        ),
        "register_user" => call!(
            register_user::register_user_service_client::RegisterUserServiceClient<Channel>,
            register_user,
            "RegisterUser"
        ),
        "user" => call!(
            user::user_service_client::UserServiceClient<Channel>,
            login,
            "ComposePost"
        ),
        "unique_id" => call!(
            uniqueid::unique_id_service_client::UniqueIdServiceClient<Channel>,
            compose_unique_id,
            "ComposePost"
        ),
        "media" => call!(
            media::media_service_client::MediaServiceClient<Channel>,
            compose_media,
            "ComposePost"
        ),
        "text" => call!(
            text_service::text_service_client::TextServiceClient<Channel>,
            compose_text,
            "ComposePost"
        ),
        "user_mention" => call!(
            user_mention::user_mention_service_client::UserMentionServiceClient<Channel>,
            compose_user_mentions,
            "ComposePost"
        ),
        "url_shorten" => call!(
            url_shorten::url_shorten_service_client::UrlShortenServiceClient<Channel>,
            compose_urls,
            "ComposePost"
        ),
        "post_storage" => call!(
            post_storage::post_storage_service_client::PostStorageServiceClient<Channel>,
            read_posts,
            "ComposePost"
        ),
        "user_timeline" => call!(
            user_timeline::user_timeline_service_client::UserTimelineServiceClient<Channel>,
            read_user_timeline,
            "ComposePost"
        ),
        "home_timeline" => call!(
            home_timeline::home_timeline_service_client::HomeTimelineServiceClient<Channel>,
            read_home_timeline,
            "ComposePost"
        ),
        "social_graph" => call!(
            social_graph::social_graph_service_client::SocialGraphServiceClient<Channel>,
            get_followers,
            "ComposePost"
        ),
        other => return Err(format!("unknown target {other}").into()),
    };
    Ok(outcome)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let masa_mode = match args.expect.as_str() {
        "masa" => true,
        "baseline" | "none" => false,
        other => {
            return Err(format!("--expect must be baseline, masa or none, got {other}").into())
        }
    };
    let check_expired = args.expect != "none";
    let timeout = Duration::from_millis(args.timeout_ms);
    let mut failures = Vec::new();

    // 0. Readiness: wait until the frontend answers a ComposePost end to end.
    let deadline = Instant::now() + Duration::from_secs(args.wait_secs);
    let mut fe = loop {
        let attempt = async {
            let ch = channel(&args.frontend, timeout).await?;
            let mut fe = frontend::frontend_service_client::FrontendServiceClient::new(ch);
            let ctx = RootContext::from(masa::create_context(
                "ComposePost",
                Duration::from_secs(5),
            ));
            fe.compose_post(ctx.attach(Request::new(compose_post_request())))
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

    // 1. Control: generous SLO, must succeed in both modes.
    let mut ok = 0;
    for _ in 0..args.control {
        let ctx = RootContext::from(masa::create_context(
            "ComposePost",
            Duration::from_secs(5),
        ));
        let req = ctx.attach(Request::new(compose_post_request()));
        match fe.compose_post(req).await {
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

    // 2. Expired deadlines, one per service.
    let mut targets = vec![("frontend".to_string(), args.frontend.clone())];
    for t in &args.targets {
        let (name, addr) = t
            .split_once('=')
            .ok_or_else(|| format!("--target must be name=addr, got {t}"))?;
        targets.push((name.to_string(), addr.to_string()));
    }
    let mut early_returns = 0;
    if check_expired {
        for (name, addr) in &targets {
            let outcome = probe(name, addr, timeout).await?;
            println!("expired-deadline {name:<15} -> {outcome:?}");
            match (&outcome, masa_mode) {
                (Outcome::EarlyReturn(msg), true) => {
                    early_returns += 1;
                    // The rejecting hop must be the service we called directly.
                    let want = format!("src={}::", service_of(name).unwrap_or("?"));
                    if !msg.contains(&want) {
                        failures.push(format!("{name}: early return from another hop: {msg}"));
                    }
                }
                (Outcome::EarlyReturn(msg), false) => {
                    early_returns += 1;
                    failures.push(format!(
                        "{name}: baseline produced a Masa early return: {msg}"
                    ));
                }
                (_, true) => {
                    failures.push(format!("{name}: expected /EarlyReturn, got {outcome:?}"))
                }
                (_, false) => {}
            }
        }
        println!(
            "policy-live: {early_returns}/{} services rejected an expired Masa deadline (expect {})",
            targets.len(),
            args.expect
        );
    }

    if failures.is_empty() {
        println!("PROBE PASS ({})", args.expect);
        Ok(())
    } else {
        for f in &failures {
            println!("FAIL: {f}");
        }
        println!("PROBE FAIL ({})", args.expect);
        std::process::exit(1);
    }
}
