//! Shared helpers for end-to-end tests.

/// Wrap a request with a Masa context (SLO and deadline), as the gateway does
/// for real traffic. In Masa mode the server hooks expect it.
#[allow(dead_code)]
pub fn with_ctx<T>(msg: T) -> tonic::Request<T> {
    use app_utils::timing::time_now;
    use masa::{ContextBuilder, MasaRequestExt};
    let slo = 10_000_000;
    let now = time_now();
    let ctx = ContextBuilder::new("test".to_string(), 0)
        .slo(slo)
        .gateway_entry(now)
        .deadline(now + slo)
        .build();
    tonic::Request::new(msg).with_masa_context(&ctx)
}
