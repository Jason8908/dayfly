use std::error::Error;
use std::iter;
use std::time::Duration;

use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockBuilder, MockServer, ResponseTemplate};

use super::{API_VERSION, API_VERSION_HEADER, Client, MEDIA_TYPE, USER_AGENT, retry};

pub(super) const TOKEN: &str = "github_pat_11DAYFLY0FAKE0TOKEN0123456789";

#[derive(Debug)]
pub(super) struct GitHub {
    server: MockServer,
}

impl GitHub {
    pub(super) async fn start() -> Self {
        Self {
            server: MockServer::start().await,
        }
    }

    pub(super) fn client(&self) -> Client {
        client_for(self.server.uri())
    }

    pub(super) async fn mount(&self, mock: Mock) {
        mock.mount(&self.server).await;
    }
}

pub(super) fn policy() -> retry::Policy {
    retry::Policy {
        max_wait: Duration::from_secs(1),
        transient_base: Duration::from_millis(1),
        throttle_floor: Duration::from_millis(1),
        ..retry::Policy::default()
    }
}

pub(super) fn client_for(base_url: String) -> Client {
    Client::build(base_url, TOKEN.parse().unwrap(), policy()).unwrap()
}

pub(super) fn request(http_method: &str, request_path: &str) -> MockBuilder {
    Mock::given(method(http_method))
        .and(path(request_path))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .and(header("accept", MEDIA_TYPE))
        .and(header(API_VERSION_HEADER, API_VERSION))
        .and(header("user-agent", USER_AGENT))
}

pub(super) fn error(status: u16, message: &str) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_json(json!({
        "message": message,
        "documentation_url": "https://docs.github.com/rest",
        "status": status.to_string(),
    }))
}

pub(super) fn assert_no_token(error: &(dyn Error + 'static)) {
    let chain: Vec<String> = iter::successors(Some(error), |&error| error.source())
        .map(ToString::to_string)
        .collect();
    let output = format!("{error:?}\n{}", chain.join("\n"));
    assert!(!output.contains(TOKEN), "{output}");
}
