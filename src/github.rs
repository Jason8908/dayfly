#[cfg(test)]
mod fake;
mod retry;

use std::fmt;
use std::str::FromStr;
use std::time::{Duration, SystemTime};

use reqwest::header::{self, HeaderMap, HeaderName, HeaderValue};
use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use retry::{Cause, Throttle};

const API_URL: &str = "https://api.github.com";
const API_VERSION: &str = "2026-03-10";
const MEDIA_TYPE: &str = "application/vnd.github+json";
const USER_AGENT: &str = concat!("dayfly/", env!("CARGO_PKG_VERSION"));
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const API_VERSION_HEADER: &str = "x-github-api-version";
const ACCEPTED_PERMISSIONS_HEADER: &str = "x-accepted-github-permissions";

#[derive(Clone)]
pub struct Token(HeaderValue);

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TokenError {
    #[error("must not be empty")]
    Empty,
    #[error("must contain only visible ASCII characters")]
    InvalidCharacter,
}

impl FromStr for Token {
    type Err = TokenError;

    fn from_str(secret: &str) -> Result<Self, Self::Err> {
        if secret.is_empty() {
            return Err(TokenError::Empty);
        }
        if !secret.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err(TokenError::InvalidCharacter);
        }
        let mut value = HeaderValue::from_str(&format!("Bearer {secret}"))
            .expect("`Bearer ` followed by visible ASCII is a valid header value");
        value.set_sensitive(true);
        Ok(Self(value))
    }
}

#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    base_url: String,
    token: Token,
    retry: retry::Policy,
}

#[derive(Debug, thiserror::Error)]
#[error("failed to set up the GitHub HTTP client")]
pub struct BuildError(#[source] reqwest::Error);

impl Client {
    pub fn new(token: Token) -> Result<Self, BuildError> {
        Self::build(API_URL.to_owned(), token, retry::Policy::default())
    }

    fn build(base_url: String, token: Token, retry: retry::Policy) -> Result<Self, BuildError> {
        let headers = HeaderMap::from_iter([
            (header::ACCEPT, HeaderValue::from_static(MEDIA_TYPE)),
            (
                HeaderName::from_static(API_VERSION_HEADER),
                HeaderValue::from_static(API_VERSION),
            ),
        ]);
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .default_headers(headers)
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(BuildError)?;
        Ok(Self {
            http,
            base_url,
            token,
            retry,
        })
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, RequestError> {
        let endpoint = Endpoint::new(Method::GET, path);
        let body = self.send(&endpoint, None).await?;
        decode(endpoint, &body)
    }

    pub async fn post<B, T>(&self, path: &str, body: &B) -> Result<T, RequestError>
    where
        B: Serialize + ?Sized,
        T: DeserializeOwned,
    {
        let endpoint = Endpoint::new(Method::POST, path);
        let body =
            serde_json::to_vec(body).expect("request bodies are plain data that serialize to JSON");
        let response = self.send(&endpoint, Some(&body)).await?;
        decode(endpoint, &response)
    }

    pub async fn delete(&self, path: &str) -> Result<(), RequestError> {
        self.send(&Endpoint::new(Method::DELETE, path), None)
            .await
            .map(drop)
    }

    async fn send(
        &self,
        endpoint: &Endpoint,
        body: Option<&[u8]>,
    ) -> Result<Vec<u8>, RequestError> {
        let url = format!("{}{}", self.base_url, endpoint.path);
        let mut retries = 0;
        loop {
            let failure = match self.attempt(&endpoint.method, &url, body).await {
                Ok(reply) if reply.status.is_success() => return Ok(reply.body),
                Ok(reply) => Failure::response(endpoint, &reply),
                Err(error) => Failure::Transport(error),
            };
            let Some(wait) = failure
                .cause(&endpoint.method)
                .and_then(|cause| self.retry.wait(cause, retries, SystemTime::now()))
            else {
                return Err(failure.into_error(endpoint.clone()));
            };
            let wait = retry::jitter(wait);
            tracing::warn!(error = %failure, ?wait, "retrying GitHub request");
            tokio::time::sleep(wait).await;
            retries += 1;
        }
    }

    async fn attempt(
        &self,
        method: &Method,
        url: &str,
        body: Option<&[u8]>,
    ) -> Result<Reply, reqwest::Error> {
        let mut request = self
            .http
            .request(method.clone(), url)
            .header(header::AUTHORIZATION, self.token.0.clone());
        if let Some(body) = body {
            request = request
                .header(header::CONTENT_TYPE, "application/json")
                .body(body.to_vec());
        }
        let response = request.send().await?;
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.bytes().await?.into();
        Ok(Reply {
            status,
            headers,
            body,
        })
    }
}

fn decode<T: DeserializeOwned>(endpoint: Endpoint, body: &[u8]) -> Result<T, RequestError> {
    serde_json::from_slice(body).map_err(|source| RequestError::Decode { endpoint, source })
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

#[derive(Deserialize)]
struct ErrorBody {
    #[serde(default)]
    message: String,
}

enum Failure {
    Response {
        response: Box<ErrorResponse>,
        throttle: Option<Throttle>,
    },
    Transport(reqwest::Error),
}

impl Failure {
    fn response(endpoint: &Endpoint, reply: &Reply) -> Self {
        let message =
            serde_json::from_slice::<ErrorBody>(&reply.body).map_or_default(|body| body.message);
        let accepted_permissions = reply
            .headers
            .get(ACCEPTED_PERMISSIONS_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        Self::Response {
            throttle: retry::throttle(reply.status, &reply.headers, &message),
            response: Box::new(ErrorResponse {
                endpoint: endpoint.clone(),
                status: reply.status,
                message,
                accepted_permissions,
            }),
        }
    }

    fn cause(&self, method: &Method) -> Option<Cause> {
        match self {
            Self::Response {
                throttle: Some(throttle),
                ..
            } => Some(Cause::Throttled(*throttle)),
            Self::Response { response, .. } => (response.status.is_server_error()
                && method.is_idempotent())
            .then_some(Cause::Transient),
            Self::Transport(error) => {
                (error.is_connect() || method.is_idempotent()).then_some(Cause::Transient)
            }
        }
    }

    fn into_error(self, endpoint: Endpoint) -> RequestError {
        match self {
            Self::Response {
                response,
                throttle: Some(_),
            } => RequestError::RateLimited(response),
            Self::Response {
                response,
                throttle: None,
            } => match response.status {
                StatusCode::UNAUTHORIZED => RequestError::Unauthorized(response),
                StatusCode::FORBIDDEN => RequestError::Forbidden(response),
                StatusCode::NOT_FOUND => RequestError::NotFound(response),
                StatusCode::UNPROCESSABLE_ENTITY => RequestError::Unprocessable(response),
                _ => RequestError::Unexpected(response),
            },
            Self::Transport(source) => RequestError::Transport { endpoint, source },
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Response { response, .. } => response.fmt(f),
            Self::Transport(error) => error.fmt(f),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RequestError {
    #[error("{0}")]
    Unauthorized(Box<ErrorResponse>),
    #[error("{0}")]
    Forbidden(Box<ErrorResponse>),
    #[error("{0}")]
    NotFound(Box<ErrorResponse>),
    #[error("{0}")]
    Unprocessable(Box<ErrorResponse>),
    #[error("{0}")]
    RateLimited(Box<ErrorResponse>),
    #[error("{0}")]
    Unexpected(Box<ErrorResponse>),
    #[error("{endpoint} failed")]
    Transport {
        endpoint: Endpoint,
        source: reqwest::Error,
    },
    #[error("{endpoint} returned a body that could not be decoded")]
    Decode {
        endpoint: Endpoint,
        source: serde_json::Error,
    },
}

#[derive(Debug, Clone)]
pub struct ErrorResponse {
    endpoint: Endpoint,
    status: StatusCode,
    message: String,
    accepted_permissions: Option<String>,
}

impl ErrorResponse {
    pub fn status(&self) -> StatusCode {
        self.status
    }

    pub fn accepted_permissions(&self) -> Option<&str> {
        self.accepted_permissions.as_deref()
    }
}

impl fmt::Display for ErrorResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} returned {}", self.endpoint, self.status)?;
        if !self.message.is_empty() {
            write!(f, ": {}", self.message)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    method: Method,
    path: String,
}

impl Endpoint {
    fn new(method: Method, path: &str) -> Self {
        Self {
            method,
            path: path.to_owned(),
        }
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.method, self.path)
    }
}

#[cfg(test)]
mod tests {
    use std::assert_matches;
    use std::net::TcpListener;
    use std::time::UNIX_EPOCH;

    use serde_json::{Value, json};
    use wiremock::ResponseTemplate;
    use wiremock::matchers::{body_json, header};

    use super::fake::{self, TOKEN};
    use super::*;

    const PATH: &str = "/repos/octo-org/octo-repo/actions/runners";

    fn attempts() -> u64 {
        u64::from(fake::policy().max_retries) + 1
    }

    async fn get(github: &fake::GitHub) -> Result<Value, RequestError> {
        github.client().get(PATH).await
    }

    async fn failing_get(response: ResponseTemplate) -> RequestError {
        let github = fake::GitHub::start().await;
        github
            .mount(fake::request("GET", PATH).respond_with(response))
            .await;
        get(&github).await.unwrap_err()
    }

    fn rate_limited(status: u16) -> ResponseTemplate {
        fake::error(status, "You have exceeded a secondary rate limit.")
    }

    fn unreachable_client() -> Client {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        fake::client_for(format!("http://{address}"))
    }

    #[test]
    fn token_rejects_empty_and_non_visible_ascii_secrets() {
        assert_matches!("".parse::<Token>(), Err(TokenError::Empty));
        for secret in ["ghp_abc def", "ghp_abc\n", "ghp_äbc", "ghp_abc\t"] {
            assert_matches!(
                secret.parse::<Token>(),
                Err(TokenError::InvalidCharacter),
                "{secret:?}"
            );
        }
    }

    #[test]
    fn token_and_client_debug_output_is_redacted() {
        let token: Token = TOKEN.parse().unwrap();
        let client = Client::new(token.clone()).unwrap();

        for output in [format!("{token:?}"), format!("{client:?}")] {
            assert!(!output.contains(TOKEN), "{output}");
        }
        assert_eq!(format!("{token:?}"), "Token(<redacted>)");
    }

    #[tokio::test]
    async fn get_sends_the_token_api_version_and_user_agent() {
        let github = fake::GitHub::start().await;
        github
            .mount(
                fake::request("GET", PATH)
                    .respond_with(
                        ResponseTemplate::new(200).set_body_json(json!({"total_count": 0})),
                    )
                    .expect(1),
            )
            .await;

        assert_eq!(get(&github).await.unwrap(), json!({"total_count": 0}));
    }

    #[tokio::test]
    async fn post_sends_a_json_body() {
        let github = fake::GitHub::start().await;
        let body = json!({"name": "dayfly-linux-1", "runner_group_id": 1});
        github
            .mount(
                fake::request("POST", PATH)
                    .and(header("content-type", "application/json"))
                    .and(body_json(&body))
                    .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 5})))
                    .expect(1),
            )
            .await;

        let created: Value = github.client().post(PATH, &body).await.unwrap();

        assert_eq!(created, json!({"id": 5}));
    }

    #[tokio::test]
    async fn delete_accepts_an_empty_response() {
        let github = fake::GitHub::start().await;
        github
            .mount(
                fake::request("DELETE", "/repos/octo-org/octo-repo/actions/runners/5")
                    .respond_with(ResponseTemplate::new(204))
                    .expect(1),
            )
            .await;

        github
            .client()
            .delete("/repos/octo-org/octo-repo/actions/runners/5")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn client_errors_map_to_typed_errors() {
        assert_matches!(
            failing_get(fake::error(401, "Bad credentials")).await,
            RequestError::Unauthorized(_)
        );
        assert_matches!(
            failing_get(fake::error(404, "Not Found")).await,
            RequestError::NotFound(_)
        );
        assert_matches!(
            failing_get(fake::error(422, "Validation Failed")).await,
            RequestError::Unprocessable(_)
        );
        assert_matches!(
            failing_get(fake::error(409, "Already exists")).await,
            RequestError::Unexpected(response) if response.status() == StatusCode::CONFLICT
        );
    }

    #[tokio::test]
    async fn forbidden_error_carries_the_permission_github_accepts() {
        let error = failing_get(
            fake::error(403, "Resource not accessible by personal access token")
                .insert_header("x-accepted-github-permissions", "administration=write")
                .insert_header("x-ratelimit-remaining", "4999"),
        )
        .await;

        assert_eq!(
            error.to_string(),
            format!(
                "GET {PATH} returned 403 Forbidden: Resource not accessible by personal access token"
            )
        );
        assert_matches!(
            error,
            RequestError::Forbidden(response)
                if response.accepted_permissions() == Some("administration=write")
        );
    }

    #[tokio::test]
    async fn server_errors_on_get_are_retried_until_success() {
        let github = fake::GitHub::start().await;
        github
            .mount(
                fake::request("GET", PATH)
                    .respond_with(fake::error(502, "Server Error"))
                    .up_to_n_times(2)
                    .expect(2),
            )
            .await;
        github
            .mount(
                fake::request("GET", PATH)
                    .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
                    .expect(1),
            )
            .await;

        assert_eq!(get(&github).await.unwrap(), json!({}));
    }

    #[tokio::test]
    async fn server_errors_on_get_stop_after_the_retry_limit() {
        let github = fake::GitHub::start().await;
        github
            .mount(
                fake::request("GET", PATH)
                    .respond_with(fake::error(500, "Server Error"))
                    .expect(attempts()),
            )
            .await;

        assert_matches!(
            get(&github).await,
            Err(RequestError::Unexpected(response)) if response.status() == StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[tokio::test]
    async fn server_errors_on_post_are_not_retried() {
        let github = fake::GitHub::start().await;
        github
            .mount(
                fake::request("POST", PATH)
                    .respond_with(fake::error(502, "Server Error"))
                    .expect(1),
            )
            .await;

        let result: Result<Value, _> = github.client().post(PATH, &json!({})).await;

        assert_matches!(result, Err(RequestError::Unexpected(_)));
    }

    #[tokio::test]
    async fn rate_limited_post_is_retried_after_the_requested_wait() {
        let github = fake::GitHub::start().await;
        github
            .mount(
                fake::request("POST", PATH)
                    .respond_with(rate_limited(403).insert_header("retry-after", "0"))
                    .up_to_n_times(1)
                    .expect(1),
            )
            .await;
        github
            .mount(
                fake::request("POST", PATH)
                    .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 5})))
                    .expect(1),
            )
            .await;

        let created: Value = github.client().post(PATH, &json!({})).await.unwrap();

        assert_eq!(created, json!({"id": 5}));
    }

    #[tokio::test]
    async fn rate_limits_stop_after_the_retry_limit() {
        let github = fake::GitHub::start().await;
        github
            .mount(
                fake::request("GET", PATH)
                    .respond_with(rate_limited(429))
                    .expect(attempts()),
            )
            .await;

        assert_matches!(get(&github).await, Err(RequestError::RateLimited(_)));
    }

    #[tokio::test]
    async fn rate_limit_resetting_after_the_longest_wait_fails_without_retrying() {
        let reset =
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap() + Duration::from_secs(3600);
        let github = fake::GitHub::start().await;
        github
            .mount(
                fake::request("GET", PATH)
                    .respond_with(
                        fake::error(403, "API rate limit exceeded")
                            .insert_header("x-ratelimit-remaining", "0")
                            .insert_header(
                                "x-ratelimit-reset",
                                reset.as_secs().to_string().as_str(),
                            ),
                    )
                    .expect(1),
            )
            .await;

        let result = tokio::time::timeout(Duration::from_secs(5), get(&github)).await;

        assert_matches!(result, Ok(Err(RequestError::RateLimited(_))));
    }

    #[tokio::test]
    async fn refused_connection_is_a_transport_error() {
        let error = unreachable_client().get::<Value>(PATH).await.unwrap_err();

        assert_matches!(&error, RequestError::Transport { .. });
        assert_eq!(error.to_string(), format!("GET {PATH} failed"));
    }

    #[tokio::test]
    async fn undecodable_success_body_is_a_decode_error() {
        assert_matches!(
            failing_get(ResponseTemplate::new(200).set_body_string("<html>")).await,
            RequestError::Decode { .. }
        );
    }

    #[tokio::test]
    async fn no_error_contains_the_token() {
        let mut errors = Vec::new();
        for status in [401, 403, 404, 409, 422, 429, 500] {
            errors.push(failing_get(fake::error(status, "message")).await);
        }
        errors.push(failing_get(ResponseTemplate::new(200).set_body_string("<html>")).await);
        errors.push(unreachable_client().get::<Value>(PATH).await.unwrap_err());

        for error in &errors {
            fake::assert_no_token(error);
        }
    }
}
