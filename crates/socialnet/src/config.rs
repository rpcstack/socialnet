//! Application configuration of the HTTP frontend that is not an environment
//! variable: the per-route SLOs, read from `config/socialnet.json` (baked into
//! the image at `/etc/socialnet`, mounted from the ConfigMap in Kubernetes).
//! This is application configuration, deliberately outside the `policy/`
//! directory so a policy cannot relax its own deadlines.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Per-route SLOs (milliseconds) the HTTP frontend gives each request's root
/// context: the request's deadline is "now + SLO" when it enters the RPC stack.
/// Every field defaults to 200 ms.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HttpSlosMs {
    /// `POST /wrk2-api/post/compose`; root-context API name `ComposePost`.
    #[serde(default = "default_slo_ms")]
    pub compose_post: u64,
    /// `POST /wrk2-api/user/register`; `RegisterUser`.
    #[serde(default = "default_slo_ms")]
    pub register_user: u64,
    /// `GET /wrk2-api/user-timeline/read`; `ReadUserTimeline`.
    #[serde(default = "default_slo_ms")]
    pub read_user_timeline: u64,
    /// `GET /wrk2-api/home-timeline/read`; `ReadHomeTimeline`.
    #[serde(default = "default_slo_ms")]
    pub read_home_timeline: u64,
    /// `POST /wrk2-api/user/follow`; `Follow`.
    #[serde(default = "default_slo_ms")]
    pub follow: u64,
    /// `POST /wrk2-api/user/unfollow`; `Unfollow`.
    #[serde(default = "default_slo_ms")]
    pub unfollow: u64,
    /// `POST /api/user/login`; `Login`.
    #[serde(default = "default_slo_ms")]
    pub login: u64,
    /// `GET /api/user/get_follower`; `GetFollowers`.
    #[serde(default = "default_slo_ms")]
    pub get_followers: u64,
    /// `GET /api/user/get_followee`; `GetFollowees`.
    #[serde(default = "default_slo_ms")]
    pub get_followees: u64,
}

impl Default for HttpSlosMs {
    fn default() -> Self {
        serde_json::from_str("{}").expect("every SLO has a default")
    }
}

fn default_slo_ms() -> u64 {
    200
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FrontendSection {
    #[serde(default)]
    slo_ms: HttpSlosMs,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct SocialnetConfig {
    #[serde(default)]
    frontend: FrontendSection,
}

/// The SLOs from `$SOCIALNET_CONFIG` (default `/etc/socialnet/socialnet.json`);
/// the defaults when that file does not exist. A file that exists but does not
/// parse is an error.
pub fn load_http_slos() -> Result<HttpSlosMs, Box<dyn std::error::Error>> {
    let path = PathBuf::from(
        std::env::var("SOCIALNET_CONFIG")
            .unwrap_or_else(|_| "/etc/socialnet/socialnet.json".to_string()),
    );
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(serde_json::from_str::<SocialnetConfig>(&text)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .frontend
            .slo_ms),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HttpSlosMs::default()),
        Err(e) => Err(format!("{}: {e}", path.display()).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_overrides() {
        assert_eq!(HttpSlosMs::default().compose_post, 200);
        let cfg: SocialnetConfig =
            serde_json::from_str(r#"{"frontend":{"sloMs":{"login":50}}}"#).unwrap();
        assert_eq!(cfg.frontend.slo_ms.login, 50);
        assert_eq!(cfg.frontend.slo_ms.follow, 200);
    }

    #[test]
    fn shipped_config_parses() {
        let text = include_str!("../../../config/socialnet.json");
        serde_json::from_str::<SocialnetConfig>(text).unwrap();
    }
}
