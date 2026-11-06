//! The frontend's public HTTP API: the routes, parameters, status codes and
//! JSON shapes of DeathStarBench socialNetwork's nginx/lua gateway
//! (`nginx-web-server/lua-scripts/wrk2-api/...` and `.../api/user/...`), so one
//! load generator (wrk2 scripts included) drives both.
//!
//! Every request enters the RPC stack the way `socialnet_client_bench` does:
//! this module creates the request's root context (API name, per-route SLO from
//! `config/socialnet.json` `frontend.sloMs`, deadline = now + SLO, stamped as
//! the gRPC `grpc-timeout`; see `app_utils::root_context`), attaches it to a
//! `FrontendServiceClient` request and calls the frontend service. That client
//! is connected in process to the very same `FrontendServiceServer` that serves
//! gRPC on the gRPC port: client hooks, the frontend's server hooks and its
//! child RPCs all run as they do for a remote gRPC client; only TCP and HTTP/2
//! framing are skipped. The handler task is spawned with the priority in the
//! request's scheduling header, as Hyper does for each HTTP/2 stream.
//!
//! Served (as in DSB; write routes read a urlencoded body, read routes the
//! query string):
//!
//! | route | DSB handler | gRPC behind it |
//! | --- | --- | --- |
//! | `/wrk2-api/post/compose` | `ComposePost` | `FrontendService.ComposePost` |
//! | `/wrk2-api/user/register` | `RegisterUserWithId` | `FrontendService.RegisterUserWithId` |
//! | `/wrk2-api/user/follow`, `/unfollow` | `Follow`/`FollowWithUsername`, ... | `FrontendService.Follow...` |
//! | `/wrk2-api/user-timeline/read` | `ReadUserTimeline` | `FrontendService.ReadUserTimeline` |
//! | `/wrk2-api/home-timeline/read` | `ReadHomeTimeline` | `FrontendService.ReadHomeTimeline` |
//! | `/api/user/login` | `Login` | `FrontendService.Login` |
//! | `/api/user/get_follower`, `/get_followee` | `GetFollowers`/`GetFollowees` | `FrontendService.GetFollowers`/`GetFollowees` |
//!
//! Not supported by this port (answered 501, never faked): the browser-facing
//! `/api/{post/compose,user/register,user/follow,user/unfollow,
//! user-timeline/read,home-timeline/read}` variants (cookie auth plus redirects
//! to the static web pages, which this port does not ship), `/api/user/user_info`
//! and the media-frontend's `/upload-media` and `/get-media`.
//!
//! Deliberate differences from the lua gateway (see README, "HTTP API"):
//! * empty lists encode as `[]` (lua's cjson encodes an empty table as `{}`);
//! * a malformed number or JSON list parameter is a 400 (lua raises, 500);
//! * a request the policy rejects or aborts is 503/504, any other failed RPC 500.

use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;

use app_utils::root_context::RootContext;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, StatusCode};
use serde::{Deserialize, Serialize};
use tonic::body::BoxBody;
use tonic::codegen::Service;
use tonic::Code;

use crate::config::HttpSlosMs;
use crate::frontend::frontend_service_client::FrontendServiceClient;
use crate::frontend::frontend_service_server::FrontendServiceServer;
use crate::frontend::server::FrontendServiceImpl;
use crate::{compose_post, home_timeline, social_graph, user, user_timeline};

/// Header carrying the opaque scheduling priority (`rpcstack_wire::SCHEDULING_HEADER`).
const SCHEDULING_HEADER: &str = "x-rpcstack-scheduling";

/// Largest request body read (DSB's nginx `client_max_body_size` default is 1m).
const MAX_BODY: usize = 1 << 20;

/// Lifetime of the login cookie and token (DSB: `cookie_ttl` 24 h in the cookie,
/// the user service's token ttl 1 h).
const COOKIE_TTL_SECS: u64 = 24 * 3600;

/// The frontend gRPC service called without a socket. Spawns the handler as a
/// task with the priority of the request's scheduling header, like Hyper's
/// HTTP/2 server does for each stream (infrastructure priority when absent).
#[derive(Clone)]
pub struct InProcess(FrontendServiceServer<FrontendServiceImpl>);

impl InProcess {
    pub fn new(server: FrontendServiceServer<FrontendServiceImpl>) -> Self {
        Self(server)
    }
}

impl Service<http::Request<BoxBody>> for InProcess {
    type Response = http::Response<BoxBody>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Infallible>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut TaskContext<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: http::Request<BoxBody>) -> Self::Future {
        let priority = req
            .headers()
            .get(SCHEDULING_HEADER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .map(tokio::task::TaskPriority::new)
            .unwrap_or_else(|| tokio::task::meta_for_unannotated_spawn(None));
        let handle = tokio::task::spawn_with_prio(self.0.call(req), priority);
        Box::pin(async move {
            Ok(match handle.await {
                Ok(Ok(response)) => response,
                Ok(Err(never)) => match never {},
                Err(e) => {
                    tonic::Status::internal(format!("frontend handler failed: {e}")).to_http()
                }
            })
        })
    }
}

pub type LocalClient = FrontendServiceClient<InProcess>;

pub struct HttpFrontend {
    client: LocalClient,
    slo: HttpSlosMs,
    jwt_secret: Option<String>,
}

impl HttpFrontend {
    pub fn new(client: LocalClient, slo: HttpSlosMs, jwt_secret: Option<String>) -> Self {
        Self {
            client,
            slo,
            jwt_secret,
        }
    }
}

pub async fn serve(addr: SocketAddr, state: Arc<HttpFrontend>) -> Result<(), hyper::Error> {
    let make = make_service_fn(move |_conn| {
        let state = state.clone();
        async move {
            Ok::<_, Infallible>(service_fn(move |req| {
                let state = state.clone();
                async move { Ok::<_, Infallible>(route(&state, req).await) }
            }))
        }
    });
    hyper::Server::bind(&addr).serve(make).await
}

// ---------------------------------------------------------------- responses

fn reply(status: StatusCode, content_type: &'static str, body: String) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("content-type", content_type)
        .body(Body::from(body))
        .unwrap()
}

/// `ngx.say`: the text and a trailing newline.
fn text(status: StatusCode, message: &str) -> Response<Body> {
    reply(status, "text/plain", format!("{message}\n"))
}

fn json<T: Serialize>(value: &T) -> Response<Body> {
    let mut body = serde_json::to_string(value).expect("serialize response");
    body.push('\n');
    reply(StatusCode::OK, "application/json; charset=utf-8", body)
}

fn incomplete() -> Response<Body> {
    text(StatusCode::BAD_REQUEST, "Incomplete arguments")
}

fn bad_request(message: &str) -> Response<Body> {
    text(StatusCode::BAD_REQUEST, message)
}

fn not_implemented(message: &str) -> Response<Body> {
    text(StatusCode::NOT_IMPLEMENTED, message)
}

/// The HTTP status of a failed RPC. Requests the policy rejects or aborts
/// (`/EarlyReturn`, admission) come back as DEADLINE_EXCEEDED, UNAVAILABLE,
/// RESOURCE_EXHAUSTED or ABORTED and keep the policy's message as the body; the
/// lua gateway answers every other failure 500 with `<prefix>: <message>`.
fn grpc_error(prefix: &str, status: &tonic::Status) -> Response<Body> {
    match status.code() {
        Code::DeadlineExceeded => text(StatusCode::GATEWAY_TIMEOUT, status.message()),
        Code::Unavailable | Code::ResourceExhausted | Code::Aborted => {
            text(StatusCode::SERVICE_UNAVAILABLE, status.message())
        }
        Code::InvalidArgument => text(
            StatusCode::BAD_REQUEST,
            &format!("{prefix}: {}", status.message()),
        ),
        _ => text(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("{prefix}: {}", status.message()),
        ),
    }
}

// The JSON of a timeline, as `_LoadTimeline` in the lua scripts builds it:
// 64-bit ids and the timestamp are strings, `post_type` the enum number.
#[derive(Serialize)]
struct PostJson {
    post_id: String,
    creator: CreatorJson,
    req_id: String,
    text: String,
    user_mentions: Vec<MentionJson>,
    media: Vec<MediaJson>,
    urls: Vec<UrlJson>,
    timestamp: String,
    post_type: i32,
}

#[derive(Serialize)]
struct CreatorJson {
    user_id: String,
    username: String,
}

#[derive(Serialize)]
struct MentionJson {
    user_id: String,
    username: String,
}

#[derive(Serialize)]
struct MediaJson {
    media_id: String,
    media_type: String,
}

#[derive(Serialize)]
struct UrlJson {
    shortened_url: String,
    expanded_url: String,
}

fn post_json(p: user_timeline::Post) -> PostJson {
    let creator = p.creator.unwrap_or_default();
    PostJson {
        post_id: p.post_id.to_string(),
        creator: CreatorJson {
            user_id: creator.user_id.to_string(),
            username: creator.username,
        },
        req_id: p.req_id.to_string(),
        text: p.text,
        user_mentions: p
            .user_mentions
            .into_iter()
            .map(|m| MentionJson {
                user_id: m.user_id.to_string(),
                username: m.username,
            })
            .collect(),
        media: p
            .media
            .into_iter()
            .map(|m| MediaJson {
                media_id: m.media_id.to_string(),
                media_type: m.media_type,
            })
            .collect(),
        urls: p
            .urls
            .into_iter()
            .map(|u| UrlJson {
                shortened_url: u.shortened_url,
                expanded_url: u.expanded_url,
            })
            .collect(),
        timestamp: p.timestamp.to_string(),
        post_type: p.post_type,
    }
}

// ------------------------------------------------------------------ routing

/// `ngx.req.get_uri_args` / `get_post_args`: the first value of each key.
struct Args(Vec<(String, String)>);

impl Args {
    fn parse(raw: &[u8]) -> Self {
        Args(form_urlencoded::parse(raw).into_owned().collect())
    }

    /// The value of `key`, `""` when absent (lua's `_StrIsEmpty` treats both alike).
    fn get(&self, key: &str) -> &str {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .unwrap_or("")
    }
}

async fn route(state: &HttpFrontend, req: Request<Body>) -> Response<Body> {
    let path = req.uri().path().to_string();
    match path.as_str() {
        "/wrk2-api/post/compose" => compose_post(state, form(req).await).await,
        "/wrk2-api/user/register" => register_user(state, form(req).await).await,
        "/wrk2-api/user/follow" => follow(state, form(req).await, true).await,
        "/wrk2-api/user/unfollow" => follow(state, form(req).await, false).await,
        "/wrk2-api/user-timeline/read" => read_timeline(state, &query(&req), true).await,
        "/wrk2-api/home-timeline/read" => read_timeline(state, &query(&req), false).await,
        "/api/user/login" => login(state, form(req).await).await,
        "/api/user/get_follower" => get_follow_list(state, &req, true).await,
        "/api/user/get_followee" => get_follow_list(state, &req, false).await,
        "/api/post/compose"
        | "/api/user/register"
        | "/api/user/follow"
        | "/api/user/unfollow"
        | "/api/user-timeline/read"
        | "/api/home-timeline/read" => not_implemented(
            "the browser-facing cookie/redirect variant is not provided by the Rust socialnet \
             port; use the /wrk2-api route",
        ),
        "/api/user/user_info" | "/upload-media" | "/get-media" => not_implemented(
            "not supported by the Rust socialnet port (no user-info RPC, no media frontend)",
        ),
        _ => text(StatusCode::NOT_FOUND, "404 Not Found"),
    }
}

fn query(req: &Request<Body>) -> Args {
    Args::parse(req.uri().query().unwrap_or("").as_bytes())
}

/// The urlencoded body (`get_post_args`); empty when absent, unreadable or too large.
async fn form(req: Request<Body>) -> Result<Args, Response<Body>> {
    let body = req.into_body();
    match hyper::body::to_bytes(body).await {
        Ok(bytes) if bytes.len() <= MAX_BODY => Ok(Args::parse(&bytes)),
        Ok(_) => Err(text(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request body too large",
        )),
        Err(_) => Err(bad_request("unreadable request body")),
    }
}

/// Lua's `tonumber` on an id: a decimal integer, else the request is malformed.
fn number(s: &str) -> Result<i64, Response<Body>> {
    s.trim()
        .parse::<i64>()
        .map_err(|_| bad_request(&format!("not a number: {s}")))
}

/// `cjson.decode` of `media_ids` / `media_types`: a JSON array whose elements
/// are numbers or numeric strings (ids) or strings (types).
fn json_list(raw: &str) -> Result<Vec<serde_json::Value>, Response<Body>> {
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(serde_json::Value::Array(items)) => Ok(items),
        _ => Err(bad_request("media_ids and media_types must be JSON arrays")),
    }
}

fn media_id(v: &serde_json::Value) -> Result<i64, Response<Body>> {
    match v {
        serde_json::Value::Number(n) => n
            .as_i64()
            .ok_or_else(|| bad_request(&format!("not a media id: {n}"))),
        serde_json::Value::String(s) => number(s),
        other => Err(bad_request(&format!("not a media id: {other}"))),
    }
}

fn media_type(v: &serde_json::Value) -> Result<String, Response<Body>> {
    match v {
        serde_json::Value::String(s) => Ok(s.clone()),
        other => Err(bad_request(&format!("not a media type: {other}"))),
    }
}

/// `POST /wrk2-api/post/compose` with `user_id`, `username`, `post_type`, `text`
/// and optionally `media_ids`, `media_types` (JSON arrays).
async fn compose_post(state: &HttpFrontend, post: Result<Args, Response<Body>>) -> Response<Body> {
    let post = match post {
        Ok(p) => p,
        Err(r) => return r,
    };
    let (user_id, username, post_type, text) = (
        post.get("user_id"),
        post.get("username"),
        post.get("post_type"),
        post.get("text"),
    );
    if [user_id, username, post_type, text]
        .iter()
        .any(|s| s.is_empty())
    {
        return incomplete();
    }
    let (user_id, post_type) = match (number(user_id), number(post_type)) {
        (Ok(u), Ok(t)) => (u, t as i32),
        (Err(r), _) | (_, Err(r)) => return r,
    };
    let (mut media_ids, mut media_types) = (Vec::new(), Vec::new());
    let (ids, types) = (post.get("media_ids"), post.get("media_types"));
    if !ids.is_empty() && !types.is_empty() {
        let parsed = json_list(ids).and_then(|ids| {
            let types = json_list(types)?;
            let ids = ids.iter().map(media_id).collect::<Result<Vec<_>, _>>()?;
            let types = types
                .iter()
                .map(media_type)
                .collect::<Result<Vec<_>, _>>()?;
            Ok((ids, types))
        });
        match parsed {
            Ok((ids, types)) => (media_ids, media_types) = (ids, types),
            Err(r) => return r,
        }
    }
    let ctx = RootContext::new("ComposePost", Duration::from_millis(state.slo.compose_post));
    let req_id = ctx.context().request_id() as i64;
    call_with_root_ctx(
        state,
        ctx,
        "compost_post failure",
        |mut c, r| async move { c.compose_post(r).await },
        compose_post::ComposePostRequest {
            req_id,
            username: username.to_string(),
            user_id,
            text: text.to_string(),
            media_ids,
            media_types,
            post_type,
            carrier: Default::default(),
        },
    )
    .await
    .map_or_else(|r| r, |_| text_ok("Successfully upload post"))
}

fn text_ok(message: &str) -> Response<Body> {
    text(StatusCode::OK, message)
}

/// Call the frontend service with `msg`, stamped with the root context `ctx`
/// (created first because the message carries the context's request id).
async fn call_with_root_ctx<T, R, F, Fut>(
    state: &HttpFrontend,
    ctx: RootContext,
    prefix: &str,
    call: F,
    msg: T,
) -> Result<R, Response<Body>>
where
    F: FnOnce(LocalClient, tonic::Request<T>) -> Fut,
    Fut: Future<Output = Result<tonic::Response<R>, tonic::Status>>,
{
    let request = ctx.attach(tonic::Request::new(msg));
    call(state.client.clone(), request)
        .await
        .map(|r| r.into_inner())
        .map_err(|status| grpc_error(prefix, &status))
}

/// `POST /wrk2-api/user/register` with `first_name`, `last_name`, `username`,
/// `password`, `user_id`.
async fn register_user(state: &HttpFrontend, post: Result<Args, Response<Body>>) -> Response<Body> {
    let post = match post {
        Ok(p) => p,
        Err(r) => return r,
    };
    let fields = ["first_name", "last_name", "username", "password", "user_id"];
    if fields.iter().any(|f| post.get(f).is_empty()) {
        return incomplete();
    }
    let user_id = match number(post.get("user_id")) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let ctx = RootContext::new(
        "RegisterUser",
        Duration::from_millis(state.slo.register_user),
    );
    let req_id = ctx.context().request_id() as i64;
    let result = call_with_root_ctx(
        state,
        ctx,
        "User registration failure",
        |mut c, r| async move { c.register_user_with_id(r).await },
        user::RegisterUserWithIdRequest {
            req_id,
            first_name: post.get("first_name").to_string(),
            last_name: post.get("last_name").to_string(),
            username: post.get("username").to_string(),
            password: post.get("password").to_string(),
            user_id,
            carrier: Default::default(),
        },
    )
    .await;
    result.map_or_else(|r| r, |_| text_ok("Success!"))
}

/// `POST /wrk2-api/user/follow` and `/unfollow`: `user_id` + `followee_id`, or
/// `user_name` + `followee_name`.
async fn follow(
    state: &HttpFrontend,
    post: Result<Args, Response<Body>>,
    follow: bool,
) -> Response<Body> {
    let post = match post {
        Ok(p) => p,
        Err(r) => return r,
    };
    let (api, slo, prefix) = if follow {
        ("Follow", state.slo.follow, "Follow Failed")
    } else {
        ("Unfollow", state.slo.unfollow, "Unfollow Failed")
    };
    let ctx = RootContext::new(api, Duration::from_millis(slo));
    let req_id = ctx.context().request_id() as i64;
    let by_id = !post.get("user_id").is_empty() && !post.get("followee_id").is_empty();
    let by_name = !post.get("user_name").is_empty() && !post.get("followee_name").is_empty();
    let carrier = Default::default();
    let result: Result<(), Response<Body>> = if by_id {
        let (user_id, followee_id) =
            match (number(post.get("user_id")), number(post.get("followee_id"))) {
                (Ok(u), Ok(f)) => (u, f),
                (Err(r), _) | (_, Err(r)) => return r,
            };
        if follow {
            let msg = social_graph::FollowRequest {
                req_id,
                user_id,
                followee_id,
                carrier,
            };
            call_with_root_ctx(
                state,
                ctx,
                prefix,
                |mut c, r| async move { c.follow(r).await },
                msg,
            )
            .await
            .map(|_| ())
        } else {
            let msg = social_graph::UnfollowRequest {
                req_id,
                user_id,
                followee_id,
                carrier,
            };
            call_with_root_ctx(
                state,
                ctx,
                prefix,
                |mut c, r| async move { c.unfollow(r).await },
                msg,
            )
            .await
            .map(|_| ())
        }
    } else if by_name {
        let user_username = post.get("user_name").to_string();
        let followee_username = post.get("followee_name").to_string();
        if follow {
            let msg = social_graph::FollowWithUsernameRequest {
                req_id,
                user_username,
                followee_username,
                carrier,
            };
            call_with_root_ctx(
                state,
                ctx,
                prefix,
                |mut c, r| async move { c.follow_with_username(r).await },
                msg,
            )
            .await
            .map(|_| ())
        } else {
            let msg = social_graph::UnfollowWithUsernameRequest {
                req_id,
                user_username,
                followee_username,
                carrier,
            };
            call_with_root_ctx(
                state,
                ctx,
                prefix,
                |mut c, r| async move { c.unfollow_with_username(r).await },
                msg,
            )
            .await
            .map(|_| ())
        }
    } else {
        return incomplete();
    };
    result.map_or_else(|r| r, |_| text_ok("Success!"))
}

/// `GET /wrk2-api/{user,home}-timeline/read?user_id=&start=&stop=`.
async fn read_timeline(state: &HttpFrontend, q: &Args, is_user: bool) -> Response<Body> {
    let (user_id, start, stop) = (q.get("user_id"), q.get("start"), q.get("stop"));
    if [user_id, start, stop].iter().any(|s| s.is_empty()) {
        return incomplete();
    }
    let (user_id, start, stop) = match (number(user_id), number(start), number(stop)) {
        (Ok(u), Ok(a), Ok(b)) => (u, a as i32, b as i32),
        (Err(r), _, _) | (_, Err(r), _) | (_, _, Err(r)) => return r,
    };
    let carrier = Default::default();
    let posts = if is_user {
        let ctx = RootContext::new(
            "ReadUserTimeline",
            Duration::from_millis(state.slo.read_user_timeline),
        );
        let req_id = ctx.context().request_id() as i64;
        let msg = user_timeline::ReadUserTimelineRequest {
            req_id,
            user_id,
            start,
            stop,
            carrier,
        };
        call_with_root_ctx(
            state,
            ctx,
            "Get user-timeline failure",
            |mut c, r| async move { c.read_user_timeline(r).await },
            msg,
        )
        .await
        .map(|r| r.posts)
    } else {
        let ctx = RootContext::new(
            "ReadHomeTimeline",
            Duration::from_millis(state.slo.read_home_timeline),
        );
        let req_id = ctx.context().request_id() as i64;
        let msg = home_timeline::ReadHomeTimelineRequest {
            req_id,
            user_id,
            start,
            stop,
            carrier,
        };
        call_with_root_ctx(
            state,
            ctx,
            "Get home-timeline failure",
            |mut c, r| async move { c.read_home_timeline(r).await },
            msg,
        )
        .await
        .map(|r| r.posts)
    };
    match posts {
        Ok(posts) => json(&posts.into_iter().map(post_json).collect::<Vec<_>>()),
        Err(r) => r,
    }
}

/// `POST /api/user/login` with `username`, `password`: on success a 302 to the
/// main page with the `login_token` cookie, as the lua gateway does.
async fn login(state: &HttpFrontend, post: Result<Args, Response<Body>>) -> Response<Body> {
    let post = match post {
        Ok(p) => p,
        Err(r) => return r,
    };
    let (username, password) = (post.get("username"), post.get("password"));
    if username.is_empty() || password.is_empty() {
        return incomplete();
    }
    let ctx = RootContext::new("Login", Duration::from_millis(state.slo.login));
    let req_id = ctx.context().request_id() as i64;
    let result = call_with_root_ctx(
        state,
        ctx,
        "User login failure",
        |mut c, r| async move { c.login(r).await },
        user::LoginRequest {
            req_id,
            username: username.to_string(),
            password: password.to_string(),
            carrier: Default::default(),
        },
    )
    .await;
    let token = match result {
        Ok(r) => r.token,
        Err(r) => return r,
    };
    let expires = httpdate::fmt_http_date(
        std::time::SystemTime::now() + Duration::from_secs(COOKIE_TTL_SECS),
    );
    let location: String = form_urlencoded::byte_serialize(username.as_bytes()).collect();
    Response::builder()
        .status(StatusCode::FOUND)
        .header("content-type", "text/plain")
        .header(
            "set-cookie",
            format!("login_token={token}; Path=/; Expires={expires}"),
        )
        .header("location", format!("../../main.html?username={location}"))
        .body(Body::empty())
        .unwrap()
}

#[derive(Deserialize)]
struct Claims {
    user_id: String,
}

/// The user id in the request's `login_token` cookie, or the 401 the lua
/// gateway answers: nothing for a missing cookie, the reason for a bad token.
fn authenticate(state: &HttpFrontend, req: &Request<Body>) -> Result<i64, Response<Body>> {
    let token = req
        .headers()
        .get_all("cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|kv| kv.trim().strip_prefix("login_token="))
        .filter(|t| !t.is_empty())
        .ok_or_else(|| reply(StatusCode::UNAUTHORIZED, "text/plain", String::new()))?;
    let secret = state.jwt_secret.as_deref().ok_or_else(|| {
        not_implemented("JWT_SECRET is not configured on the frontend; cannot verify login tokens")
    })?;
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256);
    validation.leeway = 0;
    let data = jsonwebtoken::decode::<Claims>(
        token,
        &jsonwebtoken::DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .map_err(|e| match e.kind() {
        jsonwebtoken::errors::ErrorKind::ExpiredSignature => text(
            StatusCode::UNAUTHORIZED,
            "Login token expired, please log in again",
        ),
        _ => text(StatusCode::UNAUTHORIZED, &e.to_string()),
    })?;
    data.claims
        .user_id
        .parse()
        .map_err(|_| text(StatusCode::UNAUTHORIZED, "malformed login token"))
}

#[derive(Serialize)]
struct FollowerJson {
    follower_id: String,
}

#[derive(Serialize)]
struct FolloweeJson {
    followee_id: String,
}

/// `GET /api/user/get_follower` and `/get_followee`: the ids of the logged-in
/// user's followers / followees, `[{"follower_id": "..."}]` / `[{"followee_id": ...}]`.
async fn get_follow_list(
    state: &HttpFrontend,
    req: &Request<Body>,
    followers: bool,
) -> Response<Body> {
    let user_id = match authenticate(state, req) {
        Ok(id) => id,
        Err(r) => return r,
    };
    let carrier = Default::default();
    if followers {
        let ctx = RootContext::new(
            "GetFollowers",
            Duration::from_millis(state.slo.get_followers),
        );
        let req_id = ctx.context().request_id() as i64;
        let msg = social_graph::GetFollowersRequest {
            req_id,
            user_id,
            carrier,
        };
        match call_with_root_ctx(
            state,
            ctx,
            "Get follower failure",
            |mut c, r| async move { c.get_followers(r).await },
            msg,
        )
        .await
        {
            Ok(r) => json(
                &r.user_ids
                    .iter()
                    .map(|id| FollowerJson {
                        follower_id: id.to_string(),
                    })
                    .collect::<Vec<_>>(),
            ),
            Err(r) => r,
        }
    } else {
        let ctx = RootContext::new(
            "GetFollowees",
            Duration::from_millis(state.slo.get_followees),
        );
        let req_id = ctx.context().request_id() as i64;
        let msg = social_graph::GetFolloweesRequest {
            req_id,
            user_id,
            carrier,
        };
        match call_with_root_ctx(
            state,
            ctx,
            "Get followee failure",
            |mut c, r| async move { c.get_followees(r).await },
            msg,
        )
        .await
        {
            Ok(r) => json(
                &r.user_ids
                    .iter()
                    .map(|id| FolloweeJson {
                        followee_id: id.to_string(),
                    })
                    .collect::<Vec<_>>(),
            ),
            Err(r) => r,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn body_of(r: Response<Body>) -> String {
        String::from_utf8(hyper::body::to_bytes(r.into_body()).await.unwrap().to_vec()).unwrap()
    }

    #[tokio::test]
    async fn timeline_json_matches_the_lua_shape() {
        let post = user_timeline::Post {
            post_id: 7,
            creator: Some(user::Creator {
                user_id: 1,
                username: "u1".into(),
            }),
            req_id: 9,
            text: "hi @u2".into(),
            user_mentions: vec![crate::usermention::UserMention {
                user_id: 2,
                username: "u2".into(),
            }],
            media: vec![crate::media::Media {
                media_id: 3,
                media_type: "png".into(),
            }],
            urls: vec![],
            timestamp: 1700000000000,
            post_type: 0,
        };
        let r = json(&vec![post_json(post)]);
        assert_eq!(
            body_of(r).await,
            "[{\"post_id\":\"7\",\"creator\":{\"user_id\":\"1\",\"username\":\"u1\"},\
             \"req_id\":\"9\",\"text\":\"hi @u2\",\
             \"user_mentions\":[{\"user_id\":\"2\",\"username\":\"u2\"}],\
             \"media\":[{\"media_id\":\"3\",\"media_type\":\"png\"}],\"urls\":[],\
             \"timestamp\":\"1700000000000\",\"post_type\":0}]\n"
        );
        assert_eq!(body_of(json(&Vec::<PostJson>::new())).await, "[]\n");
    }

    #[test]
    fn form_args_take_the_first_value_and_decode() {
        let a = Args::parse(b"text=a%20b+c&user_id=1&text=x");
        assert_eq!(a.get("text"), "a b c");
        assert_eq!(a.get("user_id"), "1");
        assert_eq!(a.get("missing"), "");
    }

    #[test]
    fn numbers_and_lists() {
        assert_eq!(number(" 42 ").ok(), Some(42));
        assert!(number("4x").is_err());
        let ids = json_list("[\"123\", 456]").ok().unwrap();
        assert_eq!(media_id(&ids[0]).ok(), Some(123));
        assert_eq!(media_id(&ids[1]).ok(), Some(456));
        assert!(json_list("{}").is_err());
        assert!(media_id(&serde_json::json!("abc")).is_err());
    }

    #[test]
    fn shed_requests_keep_the_policy_message() {
        let r = grpc_error("x", &tonic::Status::unavailable("/EarlyReturn?src=a"));
        assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
        let r = grpc_error("Follow Failed", &tonic::Status::not_found("nope"));
        assert_eq!(r.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
