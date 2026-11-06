//! Shared helpers for end-to-end tests.

/// A request with a generous SLO, as the gateway stamps on real traffic.
#[allow(dead_code)]
pub fn with_ctx<T>(msg: T) -> tonic::Request<T> {
    app_utils::root_context::RootContext::new("test", std::time::Duration::from_secs(10))
        .attach(tonic::Request::new(msg))
}
