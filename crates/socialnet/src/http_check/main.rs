//! HTTP API smoke check: calls every HTTP route of the frontend and validates
//! status codes and JSON shapes against what DeathStarBench's nginx/lua gateway
//! (and the vibesys generator's checks) expect. Routes the Rust port does not
//! support must answer 501 rather than pretend.
//!
//! The check builds its own data (two fresh users, a follow edge, a post), so it
//! can run against a stack in any state, any number of times.
//!
//!   socialnet_http_check --http http://frontend-service:5000 [--wait-secs 300]

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::Parser;
use hyper::header::{CONTENT_TYPE, COOKIE, LOCATION, SET_COOKIE};
use hyper::{Body, Client, Method, Request, StatusCode};
use serde_json::Value;

#[derive(Parser, Debug)]
struct Args {
    /// Base URL of the frontend's HTTP API.
    #[arg(long)]
    http: String,
    /// Retry the first request for this many seconds (readiness wait).
    #[arg(long, default_value_t = 0)]
    wait_secs: u64,
}

struct Reply {
    status: StatusCode,
    body: String,
    location: Option<String>,
    set_cookie: Option<String>,
}

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// One request; a request the policy sheds (`/EarlyReturn`, 503/504) is retried
/// a few times, since an admission policy may reject while it is cold.
async fn call(method: Method, url: &str, form: &str, cookie: Option<&str>) -> Res<Reply> {
    let mut attempt = 0;
    loop {
        let reply = call_once(method.clone(), url, form, cookie).await?;
        let shed =
            matches!(reply.status.as_u16(), 503 | 504) && reply.body.starts_with("/EarlyReturn");
        attempt += 1;
        if !shed || attempt >= 10 {
            return Ok(reply);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn call_once(method: Method, url: &str, form: &str, cookie: Option<&str>) -> Res<Reply> {
    let mut req = Request::builder().method(method).uri(url);
    if !form.is_empty() {
        req = req.header(CONTENT_TYPE, "application/x-www-form-urlencoded");
    }
    if let Some(c) = cookie {
        req = req.header(COOKIE, c);
    }
    let req = req.body(Body::from(form.to_string()))?;
    let resp = tokio::time::timeout(Duration::from_secs(10), Client::new().request(req)).await??;
    let status = resp.status();
    let header = |name| {
        resp.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    let (location, set_cookie) = (header(LOCATION), header(SET_COOKIE));
    let bytes = hyper::body::to_bytes(resp.into_body()).await?;
    Ok(Reply {
        status,
        body: String::from_utf8_lossy(&bytes).into_owned(),
        location,
        set_cookie,
    })
}

fn sorted_keys(v: &Value) -> Vec<String> {
    let mut keys: Vec<String> = v
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    keys.sort();
    keys
}

fn is_numeric_string(v: &Value) -> bool {
    v.as_str()
        .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
}

/// A timeline: a JSON array of posts in the shape of the lua `_LoadTimeline`.
fn check_timeline(body: &str) -> Result<Vec<Value>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| format!("not JSON: {e}"))?;
    let posts = v.as_array().ok_or("timeline is not an array")?;
    for p in posts {
        let want = [
            "creator",
            "media",
            "post_id",
            "post_type",
            "req_id",
            "text",
            "timestamp",
            "urls",
            "user_mentions",
        ];
        if sorted_keys(p) != want {
            return Err(format!("bad post keys: {p}"));
        }
        let ok = ["post_id", "req_id", "timestamp"]
            .iter()
            .all(|k| is_numeric_string(&p[k]))
            && sorted_keys(&p["creator"]) == ["user_id", "username"]
            && is_numeric_string(&p["creator"]["user_id"])
            && p["text"].is_string()
            && p["post_type"].is_i64()
            && p["user_mentions"].is_array()
            && p["media"].is_array()
            && p["urls"].is_array();
        if !ok {
            return Err(format!("bad post: {p}"));
        }
        for m in p["media"].as_array().unwrap() {
            if sorted_keys(m) != ["media_id", "media_type"] || !is_numeric_string(&m["media_id"]) {
                return Err(format!("bad media: {m}"));
            }
        }
        for m in p["user_mentions"].as_array().unwrap() {
            if sorted_keys(m) != ["user_id", "username"] {
                return Err(format!("bad mention: {m}"));
            }
        }
        for u in p["urls"].as_array().unwrap() {
            if sorted_keys(u) != ["expanded_url", "shortened_url"] {
                return Err(format!("bad url: {u}"));
            }
        }
    }
    Ok(posts.clone())
}

/// `[{"<key>": "<id>"}, ...]`, the ids it holds.
fn check_id_list(body: &str, key: &str) -> Result<Vec<String>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| format!("not JSON: {e}"))?;
    let items = v.as_array().ok_or("not an array")?;
    items
        .iter()
        .map(|i| {
            if sorted_keys(i) == [key] && is_numeric_string(&i[key]) {
                Ok(i[key].as_str().unwrap().to_string())
            } else {
                Err(format!("bad item {i}"))
            }
        })
        .collect()
}

fn enc(s: &str) -> String {
    form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", enc(k), enc(v)))
        .collect::<Vec<_>>()
        .join("&")
}

struct Checker {
    failures: Vec<String>,
}

impl Checker {
    /// Assert a status and run `check` on the body.
    fn expect(
        &mut self,
        name: &str,
        got: Res<Reply>,
        status: u16,
        check: &dyn Fn(&Reply) -> Result<String, String>,
    ) {
        let line = match got {
            Err(e) => Err(format!("request failed: {e}")),
            Ok(r) if r.status.as_u16() != status => Err(format!(
                "status {} (want {status}): {}",
                r.status,
                r.body.trim()
            )),
            Ok(r) => check(&r),
        };
        match line {
            Ok(note) => println!("ok   {name}: {note}"),
            Err(e) => {
                println!("FAIL {name}: {e}");
                self.failures.push(name.to_string());
            }
        }
    }

    /// Assert a plain status and body text.
    fn expect_text(&mut self, name: &str, got: Res<Reply>, status: u16, body: &str) {
        let body = body.to_string();
        self.expect(name, got, status, &move |r| {
            if body.is_empty() || r.body.trim() == body {
                Ok(r.body.trim().to_string())
            } else {
                Err(format!("body {:?}, want {body:?}", r.body.trim()))
            }
        });
    }
}

fn cookie_of(set_cookie: &str) -> String {
    set_cookie.split(';').next().unwrap_or("").to_string()
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Res<()> {
    let args = Args::parse();
    let base = args.http.trim_end_matches('/').to_string();
    let mut c = Checker { failures: vec![] };

    let probe = format!("{base}/wrk2-api/user-timeline/read?user_id=1&start=0&stop=1");
    let deadline = Instant::now() + Duration::from_secs(args.wait_secs);
    loop {
        match call(Method::GET, &probe, "", None).await {
            Ok(r) if r.status == StatusCode::OK => break,
            other if Instant::now() < deadline => {
                println!(
                    "waiting for the HTTP frontend: {:?}",
                    other.map(|r| r.status)
                );
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Ok(r) => return Err(format!("frontend not ready: {} {}", r.status, r.body).into()),
            Err(e) => return Err(format!("frontend not ready: {e}").into()),
        }
    }

    // Fresh users: `a` posts, `b` follows `a`.
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let (a_name, b_name) = (format!("hc_{nanos}_a"), format!("hc_{nanos}_b"));
    let base_id = 4_000_000_000_000 + (nanos % 1_000_000_000) as i64 * 100;
    let (a_id, b_id) = (base_id.to_string(), (base_id + 1).to_string());
    let register = |name: &str, id: &str| {
        form(&[
            ("first_name", "Http"),
            ("last_name", "Check"),
            ("username", name),
            ("password", "pw"),
            ("user_id", id),
        ])
    };

    // ---- register
    let url = format!("{base}/wrk2-api/user/register");
    c.expect_text(
        "POST register a",
        call(Method::POST, &url, &register(&a_name, &a_id), None).await,
        200,
        "Success!",
    );
    c.expect_text(
        "POST register b",
        call(Method::POST, &url, &register(&b_name, &b_id), None).await,
        200,
        "Success!",
    );
    c.expect(
        "POST register (duplicate username)",
        call(Method::POST, &url, &register(&a_name, &a_id), None).await,
        500,
        &|r| {
            if r.body.starts_with("User registration failure") {
                Ok(r.body.trim().to_string())
            } else {
                Err(format!("unexpected body {:?}", r.body))
            }
        },
    );
    c.expect_text(
        "POST register (missing user_id)",
        call(
            Method::POST,
            &url,
            &form(&[
                ("first_name", "x"),
                ("last_name", "y"),
                ("username", "u"),
                ("password", "p"),
            ]),
            None,
        )
        .await,
        400,
        "Incomplete arguments",
    );

    // ---- login
    let url = format!("{base}/api/user/login");
    let login = |name: &str, pw: &str| form(&[("username", name), ("password", pw)]);
    let mut cookie_a = String::new();
    let mut cookie_b = String::new();
    for (who, name, out) in [("a", &a_name, &mut cookie_a), ("b", &b_name, &mut cookie_b)] {
        let got = call(Method::POST, &url, &login(name, "pw"), None).await;
        c.expect(
            &format!("POST login {who}"),
            got.map(|r| {
                *out = r.set_cookie.as_deref().map(cookie_of).unwrap_or_default();
                r
            }),
            302,
            &|r| {
                let cookie_ok = r
                    .set_cookie
                    .as_deref()
                    .is_some_and(|c| c.starts_with("login_token=") && c.contains("Path=/"));
                let loc_ok = r
                    .location
                    .as_deref()
                    .is_some_and(|l| l.starts_with("../../main.html?username="));
                if cookie_ok && loc_ok {
                    Ok("login_token cookie and redirect".to_string())
                } else {
                    Err(format!(
                        "cookie {:?}, location {:?}",
                        r.set_cookie, r.location
                    ))
                }
            },
        );
    }
    c.expect(
        "POST login (wrong password)",
        call(Method::POST, &url, &login(&a_name, "nope"), None).await,
        500,
        &|r| {
            if r.body.starts_with("User login failure") {
                Ok(r.body.trim().to_string())
            } else {
                Err(format!("unexpected body {:?}", r.body))
            }
        },
    );
    c.expect_text(
        "POST login (no password)",
        call(Method::POST, &url, &form(&[("username", &a_name)]), None).await,
        400,
        "Incomplete arguments",
    );

    // ---- follow, followers, followees, unfollow
    let follow = format!("{base}/wrk2-api/user/follow");
    let unfollow = format!("{base}/wrk2-api/user/unfollow");
    c.expect_text(
        "POST follow (ids)",
        call(
            Method::POST,
            &follow,
            &form(&[("user_id", &b_id), ("followee_id", &a_id)]),
            None,
        )
        .await,
        200,
        "Success!",
    );
    c.expect_text(
        "POST follow (missing followee)",
        call(Method::POST, &follow, &form(&[("user_id", &b_id)]), None).await,
        400,
        "Incomplete arguments",
    );
    let followers = format!("{base}/api/user/get_follower");
    let followees = format!("{base}/api/user/get_followee");
    c.expect(
        "GET get_follower (a, after follow)",
        call(Method::GET, &followers, "", Some(&cookie_a)).await,
        200,
        &|r| {
            let ids = check_id_list(&r.body, "follower_id")?;
            if ids.contains(&b_id) {
                Ok(format!("{} follower(s)", ids.len()))
            } else {
                Err(format!("{b_id} missing from {ids:?}"))
            }
        },
    );
    c.expect(
        "GET get_followee (b, after follow)",
        call(Method::GET, &followees, "", Some(&cookie_b)).await,
        200,
        &|r| {
            let ids = check_id_list(&r.body, "followee_id")?;
            if ids.contains(&a_id) {
                Ok(format!("{} followee(s)", ids.len()))
            } else {
                Err(format!("{a_id} missing from {ids:?}"))
            }
        },
    );
    c.expect_text(
        "GET get_follower (no cookie)",
        call(Method::GET, &followers, "", None).await,
        401,
        "",
    );
    c.expect(
        "GET get_follower (bad token)",
        call(Method::GET, &followers, "", Some("login_token=garbage")).await,
        401,
        &|r| Ok(r.body.trim().to_string()),
    );

    // ---- compose, then read both timelines
    let compose = format!("{base}/wrk2-api/post/compose");
    let text = format!("hello @{b_name} see http://example.com/hc{nanos}");
    let post = form(&[
        ("user_id", &a_id),
        ("username", &a_name),
        ("post_type", "0"),
        ("text", &text),
        ("media_ids", "[\"11\",\"12\"]"),
        ("media_types", "[\"png\",\"jpg\"]"),
    ]);
    c.expect_text(
        "POST compose",
        call(Method::POST, &compose, &post, None).await,
        200,
        "Successfully upload post",
    );
    c.expect_text(
        "POST compose (no media)",
        call(
            Method::POST,
            &compose,
            &form(&[
                ("user_id", &a_id),
                ("username", &a_name),
                ("post_type", "0"),
                ("text", "plain"),
            ]),
            None,
        )
        .await,
        200,
        "Successfully upload post",
    );
    c.expect_text(
        "POST compose (missing text)",
        call(
            Method::POST,
            &compose,
            &form(&[
                ("user_id", &a_id),
                ("username", &a_name),
                ("post_type", "0"),
            ]),
            None,
        )
        .await,
        400,
        "Incomplete arguments",
    );
    c.expect_text(
        "POST compose (bad media_ids)",
        call(
            Method::POST,
            &compose,
            &form(&[
                ("user_id", &a_id),
                ("username", &a_name),
                ("post_type", "0"),
                ("text", "x"),
                ("media_ids", "oops"),
                ("media_types", "[]"),
            ]),
            None,
        )
        .await,
        400,
        "",
    );
    let (a_id2, a_name2) = (a_id.clone(), a_name.clone());
    let read_user = format!("{base}/wrk2-api/user-timeline/read?user_id={a_id}&start=0&stop=10");
    c.expect(
        "GET user-timeline/read",
        call(Method::GET, &read_user, "", None).await,
        200,
        &move |r| {
            let posts = check_timeline(&r.body)?;
            if posts.len() < 2 {
                return Err(format!("{} posts, want at least 2", posts.len()));
            }
            // Newest first: the plain post, then the one with media.
            let with_media = posts
                .iter()
                .find(|p| p["media"].as_array().is_some_and(|m| !m.is_empty()));
            let with_media = with_media.ok_or("the post with media is missing")?;
            let media: Vec<&str> = with_media["media"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|m| m["media_id"].as_str())
                .collect();
            if media != ["11", "12"] {
                return Err(format!("media {media:?}"));
            }
            if posts[0]["creator"]["user_id"] != a_id2.as_str()
                || posts[0]["creator"]["username"] != a_name2.as_str()
            {
                return Err(format!("creator {}", posts[0]["creator"]));
            }
            if posts[0]["text"] != "plain" {
                return Err(format!("newest post is {}", posts[0]["text"]));
            }
            Ok(format!("{} posts", posts.len()))
        },
    );
    c.expect(
        "GET user-timeline/read (empty range)",
        call(
            Method::GET,
            &format!("{base}/wrk2-api/user-timeline/read?user_id={a_id}&start=5&stop=5"),
            "",
            None,
        )
        .await,
        200,
        &|r| check_timeline(&r.body).map(|p| format!("{} posts", p.len())),
    );
    c.expect_text(
        "GET user-timeline/read (no stop)",
        call(
            Method::GET,
            &format!("{base}/wrk2-api/user-timeline/read?user_id=1&start=0"),
            "",
            None,
        )
        .await,
        400,
        "Incomplete arguments",
    );
    let read_home = format!("{base}/wrk2-api/home-timeline/read?user_id={b_id}&start=0&stop=10");
    c.expect(
        "GET home-timeline/read (follower b)",
        call(Method::GET, &read_home, "", None).await,
        200,
        &move |r| {
            let posts = check_timeline(&r.body)?;
            if posts.len() >= 2 {
                Ok(format!("{} posts", posts.len()))
            } else {
                Err(format!("{} posts, want at least 2", posts.len()))
            }
        },
    );
    c.expect_text(
        "GET home-timeline/read (no user_id)",
        call(
            Method::GET,
            &format!("{base}/wrk2-api/home-timeline/read?start=0&stop=1"),
            "",
            None,
        )
        .await,
        400,
        "Incomplete arguments",
    );

    // ---- unfollow (by username this time), then the lists shrink
    c.expect_text(
        "POST unfollow (usernames)",
        call(
            Method::POST,
            &unfollow,
            &form(&[("user_name", &b_name), ("followee_name", &a_name)]),
            None,
        )
        .await,
        200,
        "Success!",
    );
    c.expect_text(
        "POST unfollow (nothing)",
        call(Method::POST, &unfollow, "", None).await,
        400,
        "Incomplete arguments",
    );
    c.expect(
        "GET get_follower (a, after unfollow)",
        call(Method::GET, &followers, "", Some(&cookie_a)).await,
        200,
        &|r| {
            let ids = check_id_list(&r.body, "follower_id")?;
            if ids.contains(&b_id) {
                Err(format!("{b_id} still in {ids:?}"))
            } else {
                Ok(format!("{} follower(s)", ids.len()))
            }
        },
    );
    c.expect_text(
        "POST follow (usernames)",
        call(
            Method::POST,
            &follow,
            &form(&[("user_name", &b_name), ("followee_name", &a_name)]),
            None,
        )
        .await,
        200,
        "Success!",
    );

    // ---- routes the port does not serve
    for route in [
        "/api/post/compose",
        "/api/user/register",
        "/api/user/follow",
        "/api/user/unfollow",
        "/api/user-timeline/read",
        "/api/home-timeline/read",
        "/api/user/user_info",
        "/upload-media",
        "/get-media",
    ] {
        c.expect_text(
            &format!("GET {route} (unsupported)"),
            call(Method::GET, &format!("{base}{route}"), "", None).await,
            501,
            "",
        );
    }
    c.expect_text(
        "GET /nope",
        call(Method::GET, &format!("{base}/nope"), "", None).await,
        404,
        "",
    );

    if c.failures.is_empty() {
        println!("HTTP CHECK PASS");
        Ok(())
    } else {
        println!("HTTP CHECK FAIL: {}", c.failures.join(", "));
        std::process::exit(1);
    }
}
