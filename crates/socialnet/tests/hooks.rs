//! Regression test: every generated stub must default to `masa::DefaultHooks`.
//!
//! rpcstack's tonic-build defaults generated stubs to
//! `tonic::rpcstack::noop::NoopHooks`. If `build.rs` forgets
//! `.default_hooks_path("masa::DefaultHooks")` for any proto, that service runs
//! without the policy and nothing else fails. These checks compare the stubs'
//! default hook type parameter against `masa::DefaultHooks`, and in Masa mode
//! also check that `masa::DefaultHooks` is not the no-op hooks.

use std::any::TypeId;

#[allow(unused_imports)]
use socialnet::*;
use tonic::rpcstack::noop::NoopHooks;
use tonic::transport::Channel;

const PROTOS: &[&str] = &[
    "compose_post",
    "post_storage",
    "uniqueidservice",
    "usermention",
    "frontend",
    "home_timeline",
    "media",
    "register_user",
    "social_graph",
    "textservice",
    "url_shorten",
    "user_timeline",
    "user",
];

/// Type-level check for clients: `XClient<Channel>` (default hook parameter)
/// must be the same type as `XClient<Channel, masa::DefaultHooks>`.
macro_rules! assert_client_default {
    ($m:ident :: $client_mod:ident :: $client:ident) => {
        assert_eq!(
            TypeId::of::<$m::$client_mod::$client<Channel>>(),
            TypeId::of::<$m::$client_mod::$client<Channel, masa::DefaultHooks>>(),
            "{} does not default to masa::DefaultHooks",
            stringify!($client)
        );
    };
}

#[test]
fn generated_clients_default_to_masa_hooks() {
    assert_client_default!(media::media_service_client::MediaServiceClient);
    assert_client_default!(url_shorten::url_shorten_service_client::UrlShortenServiceClient);
    assert_client_default!(user_mention::user_mention_service_client::UserMentionServiceClient);
    assert_client_default!(text_service::text_service_client::TextServiceClient);
    assert_client_default!(user::user_service_client::UserServiceClient);
    assert_client_default!(user_timeline::user_timeline_service_client::UserTimelineServiceClient);
    assert_client_default!(post_storage::post_storage_service_client::PostStorageServiceClient);
    assert_client_default!(compose_post::compose_post_service_client::ComposePostServiceClient);
    assert_client_default!(register_user::register_user_service_client::RegisterUserServiceClient);
    assert_client_default!(frontend::frontend_service_client::FrontendServiceClient);
}

/// Source-level check covering servers too (their type needs a service impl,
/// which lives in the binaries): every default hook parameter in the generated
/// code must be `masa::DefaultHooks`, one for each client and each server.
#[test]
fn generated_sources_default_to_masa_hooks() {
    for proto in PROTOS {
        let path = std::path::Path::new(env!("OUT_DIR")).join(format!("{proto}.rs"));
        let src = std::fs::read_to_string(&path).expect("generated source");
        assert!(
            !src.contains("NoopHooks"),
            "{proto}: generated code names NoopHooks"
        );
        let defaults: Vec<&str> = src
            .split("tonic::rpcstack::Hooks =")
            .skip(1)
            .map(|rest| {
                rest.trim_start()
                    .split(|c: char| c == '>' || c == ',')
                    .next()
                    .unwrap()
                    .trim()
            })
            .collect();
        assert_eq!(
            defaults.len(),
            2,
            "{proto}: expected a client and a server default, got {defaults:?}"
        );
        for d in defaults {
            assert_eq!(d, "masa::DefaultHooks", "{proto}: default hooks are {d}");
        }
    }
}

/// In Masa mode the generated stubs must actually run the policy.
#[cfg(feature = "masa")]
#[test]
fn masa_mode_does_not_fall_back_to_noop_hooks() {
    assert_ne!(
        TypeId::of::<masa::DefaultHooks>(),
        TypeId::of::<NoopHooks>(),
        "masa feature is on but masa::DefaultHooks resolved to NoopHooks"
    );
    assert_ne!(
        TypeId::of::<frontend::frontend_service_client::FrontendServiceClient<Channel>>(),
        TypeId::of::<frontend::frontend_service_client::FrontendServiceClient<Channel, NoopHooks>>(
        ),
    );
}

/// Baseline mode is the no-op hook stack: stubs pay no policy cost.
#[cfg(not(any(
    feature = "sched_fifo",
    feature = "sched_slo",
    feature = "sched_tailclipper",
    feature = "sched_oracle"
)))]
#[test]
fn baseline_mode_uses_noop_hooks() {
    assert_eq!(
        TypeId::of::<masa::DefaultHooks>(),
        TypeId::of::<NoopHooks>()
    );
}
