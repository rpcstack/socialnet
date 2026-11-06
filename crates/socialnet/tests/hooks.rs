//! Regression test: every generated stub must default to the mode's hooks.
//!
//! rpcstack's tonic-build defaults generated stubs to
//! `tonic::rpcstack::noop::NoopHooks`. If `build.rs` forgets
//! `.default_hooks_path(...)` for any proto, that service runs without the
//! policy and nothing else fails. These checks compare the stubs' default hook
//! type parameter against the mode's hooks: the framework's `NoopHooks` in
//! `baseline` mode, `socialnet_policy::Hooks` (the `policy/` directory's stack) in
//! `policy` mode; and that in policy mode those hooks are not the no-op hooks.
//! (`build.rs` also fails a policy build whose stub names `NoopHooks`.)

use std::any::TypeId;

#[allow(unused_imports)]
#[allow(unused_imports)]
use socialnet::*;
use tonic::rpcstack::noop::NoopHooks;
use tonic::transport::Channel;

/// The hooks the stubs of this build must default to.
#[cfg(feature = "policy")]
type ModeHooks = socialnet_policy::Hooks;
#[cfg(not(feature = "policy"))]
type ModeHooks = NoopHooks;
#[cfg(feature = "policy")]
const MODE_HOOKS_PATH: &str = "socialnet_policy::Hooks";
#[cfg(not(feature = "policy"))]
const MODE_HOOKS_PATH: &str = "tonic::rpcstack::noop::NoopHooks";

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
/// must be the same type as `XClient<Channel, ModeHooks>`.
macro_rules! assert_client_default {
    ($m:ident :: $client_mod:ident :: $client:ident) => {
        assert_eq!(
            TypeId::of::<$m::$client_mod::$client<Channel>>(),
            TypeId::of::<$m::$client_mod::$client<Channel, ModeHooks>>(),
            "{} does not default to {}",
            stringify!($client),
            MODE_HOOKS_PATH
        );
    };
}

#[test]
fn generated_clients_default_to_mode_hooks() {
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
/// code must be the mode's hooks, one for each client and each server.
#[test]
fn generated_sources_default_to_mode_hooks() {
    for proto in PROTOS {
        let path = std::path::Path::new(env!("OUT_DIR")).join(format!("{proto}.rs"));
        let src = std::fs::read_to_string(&path).expect("generated source");
        #[cfg(feature = "policy")]
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
            assert_eq!(d, MODE_HOOKS_PATH, "{proto}: default hooks are {d}");
        }
    }
}

/// In policy mode the stubs run the policy directory's stack, which is neither
/// the no-op hooks nor (necessarily) Masa's default; every client must carry it.
#[cfg(feature = "policy")]
#[test]
fn policy_mode_stubs_run_the_policy_directory_stack() {
    assert_ne!(
        TypeId::of::<socialnet_policy::Hooks>(),
        TypeId::of::<NoopHooks>(),
        "policy mode but socialnet_policy::Hooks is the no-op hooks"
    );
    assert_ne!(
        TypeId::of::<frontend::frontend_service_client::FrontendServiceClient<Channel>>(),
        TypeId::of::<frontend::frontend_service_client::FrontendServiceClient<Channel, NoopHooks>>(
        ),
    );
    // The stack must be the declared one, not a stand-in type with the same name.
    assert_eq!(
        TypeId::of::<ModeHooks>(),
        TypeId::of::<rpcstack_tonic::PolicyHooks<socialnet_policy::Stack>>()
    );
}

/// In policy mode the run queue Tokio uses is the policy directory's
/// `queue.rs`, not the framework's `custom.rs` or FIFO/priority default.
#[cfg(feature = "policy")]
#[test]
fn policy_mode_run_queue_is_the_policy_queue() {
    assert_eq!(
        rpcstack_sched::external::POLICY_QUEUE_ID,
        socialnet_policy::queue::POLICY_QUEUE_ID
    );
    assert_eq!(
        TypeId::of::<rpcstack_sched::SelectedQueue<u64>>(),
        TypeId::of::<rpcstack_sched::external::Queue<u64>>(),
    );
}

/// Baseline mode is the no-op hook stack: stubs pay no policy cost.
#[cfg(not(feature = "policy"))]
#[test]
fn baseline_mode_uses_noop_hooks() {
    assert_eq!(TypeId::of::<ModeHooks>(), TypeId::of::<NoopHooks>());
}
