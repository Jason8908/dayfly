use std::fmt;
use std::iter;

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use crate::config::{Label, PoolName, Scope};

use super::{Client, RequestError, runners_path};

const NAME_PREFIX: &str = "dayfly";
const NAME_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
const NAME_SUFFIX_LEN: usize = 8;
const MAX_NAME_ATTEMPTS: u32 = 3;

#[derive(Debug, Clone, Copy)]
pub struct NewRunner<'a> {
    pub pool: &'a PoolName,
    pub labels: &'a [Label],
    pub runner_group_id: u64,
    pub work_folder: &'a str,
}

#[derive(Debug)]
pub struct JitRunner {
    id: RunnerId,
    name: RunnerName,
    config: JitConfig,
}

impl JitRunner {
    pub fn id(&self) -> RunnerId {
        self.id
    }

    pub fn name(&self) -> &RunnerName {
        &self.name
    }

    pub fn config(&self) -> &JitConfig {
        &self.config
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(transparent)]
pub struct RunnerId(u64);

impl fmt::Display for RunnerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RunnerName(String);

impl RunnerName {
    fn generate(pool: &PoolName) -> Self {
        let suffix: String =
            iter::repeat_with(|| char::from(NAME_ALPHABET[fastrand::usize(..NAME_ALPHABET.len())]))
                .take(NAME_SUFFIX_LEN)
                .collect();
        Self(format!("{NAME_PREFIX}-{pool}-{suffix}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RunnerName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Deserialize)]
#[serde(transparent)]
pub struct JitConfig(String);

impl JitConfig {
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for JitConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("JitConfig(<redacted>)")
    }
}

#[derive(Serialize)]
struct JitConfigRequest<'a> {
    name: &'a str,
    runner_group_id: u64,
    labels: &'a [&'a str],
    work_folder: &'a str,
}

#[derive(Deserialize)]
struct JitConfigResponse {
    runner: RegisteredRunner,
    encoded_jit_config: JitConfig,
}

#[derive(Deserialize)]
struct RegisteredRunner {
    id: RunnerId,
}

impl Client {
    pub async fn generate_jit_config(
        &self,
        scope: &Scope,
        runner: &NewRunner<'_>,
    ) -> Result<JitRunner, RequestError> {
        let path = format!("{}/generate-jitconfig", runners_path(scope));
        let labels: Vec<&str> = runner.labels.iter().map(Label::as_str).collect();
        let mut attempt = 1;
        loop {
            let name = RunnerName::generate(runner.pool);
            let request = JitConfigRequest {
                name: name.as_str(),
                runner_group_id: runner.runner_group_id,
                labels: &labels,
                work_folder: runner.work_folder,
            };
            match self.post::<_, JitConfigResponse>(&path, &request).await {
                Ok(response) => {
                    return Ok(JitRunner {
                        id: response.runner.id,
                        name,
                        config: response.encoded_jit_config,
                    });
                }
                Err(RequestError::Unexpected(response))
                    if response.status() == StatusCode::CONFLICT && attempt < MAX_NAME_ATTEMPTS =>
                {
                    tracing::warn!(
                        runner_name = %name,
                        error = %response,
                        "runner name is taken, retrying with a new name"
                    );
                    attempt += 1;
                }
                Err(error) => return Err(error),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use serde_json::{Value, json};
    use wiremock::matchers::body_partial_json;
    use wiremock::{Request, ResponseTemplate};

    use super::super::fake;
    use super::*;

    const REPOSITORY_PATH: &str = "/repos/octo-org/octo-repo/actions/runners/generate-jitconfig";
    const ORGANIZATION_PATH: &str = "/orgs/octo-org/actions/runners/generate-jitconfig";
    const ENCODED_JIT_CONFIG: &str = "eyJkYXlmbHkiOiJmYWtlIEpJVCBjb25maWcifQ==";
    const LABELS: [&str; 3] = ["self-hosted", "linux", "x64"];

    fn repository() -> Scope {
        Scope::Repository("octo-org/octo-repo".parse().unwrap())
    }

    fn organization() -> Scope {
        Scope::Organization("octo-org".parse().unwrap())
    }

    fn pool() -> PoolName {
        "linux".parse().unwrap()
    }

    fn created(request: &Request) -> ResponseTemplate {
        let body: Value = request.body_json().unwrap();
        ResponseTemplate::new(201).set_body_json(json!({
            "runner": {
                "id": 23,
                "name": body["name"],
                "os": "unknown",
                "status": "offline",
                "busy": false,
                "labels": [],
                "ephemeral": true,
            },
            "encoded_jit_config": ENCODED_JIT_CONFIG,
        }))
    }

    fn name_taken() -> ResponseTemplate {
        fake::error(409, "Already exists")
    }

    async fn generate(github: &fake::GitHub, scope: &Scope) -> Result<JitRunner, RequestError> {
        let labels = LABELS.map(|label| label.parse().unwrap());
        let runner = NewRunner {
            pool: &pool(),
            labels: &labels,
            runner_group_id: 1,
            work_folder: "_work",
        };
        github.client().generate_jit_config(scope, &runner).await
    }

    async fn names_sent(github: &fake::GitHub) -> Vec<String> {
        github
            .requests()
            .await
            .iter()
            .map(|request| {
                let body: Value = request.body_json().unwrap();
                body["name"].as_str().unwrap().to_owned()
            })
            .collect()
    }

    async fn assert_generates_a_config(scope: Scope, path: &str) {
        let github = fake::GitHub::start().await;
        github
            .mount(
                fake::request("POST", path)
                    .and(body_partial_json(json!({
                        "runner_group_id": 1,
                        "labels": LABELS,
                        "work_folder": "_work",
                    })))
                    .respond_with(created)
                    .expect(1),
            )
            .await;

        let runner = generate(&github, &scope).await.unwrap();

        assert_eq!(runner.id(), RunnerId(23));
        assert_eq!(runner.config().expose_secret(), ENCODED_JIT_CONFIG);
        assert_eq!(names_sent(&github).await, [runner.name().as_str()]);
    }

    #[test]
    fn runner_names_are_the_pool_name_and_a_random_lowercase_suffix() {
        let name = RunnerName::generate(&pool());

        let suffix = name.as_str().strip_prefix("dayfly-linux-").unwrap();
        assert_eq!(suffix.len(), 8, "{name}");
        assert!(
            suffix
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit()),
            "{name}"
        );
        assert_ne!(RunnerName::generate(&pool()), name);
    }

    #[tokio::test]
    async fn repository_runner_gets_a_jit_config() {
        assert_generates_a_config(repository(), REPOSITORY_PATH).await;
    }

    #[tokio::test]
    async fn organization_runner_gets_a_jit_config() {
        assert_generates_a_config(organization(), ORGANIZATION_PATH).await;
    }

    #[tokio::test]
    async fn taken_name_is_retried_with_a_new_name() {
        let github = fake::GitHub::start().await;
        github
            .mount(
                fake::request("POST", REPOSITORY_PATH)
                    .respond_with(name_taken())
                    .up_to_n_times(1)
                    .expect(1),
            )
            .await;
        github
            .mount(
                fake::request("POST", REPOSITORY_PATH)
                    .respond_with(created)
                    .expect(1),
            )
            .await;

        let runner = generate(&github, &repository()).await.unwrap();

        assert_matches!(
            names_sent(&github).await.as_slice(),
            [first, second] if first != second && second == runner.name().as_str()
        );
    }

    #[tokio::test]
    async fn taken_names_stop_after_the_attempt_limit() {
        let github = fake::GitHub::start().await;
        github
            .mount(
                fake::request("POST", REPOSITORY_PATH)
                    .respond_with(name_taken())
                    .expect(u64::from(MAX_NAME_ATTEMPTS)),
            )
            .await;

        assert_matches!(
            generate(&github, &repository()).await,
            Err(RequestError::Unexpected(response)) if response.status() == StatusCode::CONFLICT
        );
    }

    #[tokio::test]
    async fn other_failures_are_not_retried_with_a_new_name() {
        let github = fake::GitHub::start().await;
        github
            .mount(
                fake::request("POST", REPOSITORY_PATH)
                    .respond_with(fake::error(422, "Validation Failed"))
                    .expect(1),
            )
            .await;

        assert_matches!(
            generate(&github, &repository()).await,
            Err(RequestError::Unprocessable(_))
        );
    }

    #[tokio::test]
    async fn jit_config_never_appears_in_debug_output_or_errors() {
        let github = fake::GitHub::start().await;
        github
            .mount(
                fake::request("POST", REPOSITORY_PATH)
                    .respond_with(created)
                    .up_to_n_times(1),
            )
            .await;
        github
            .mount(fake::request("POST", REPOSITORY_PATH).respond_with(
                ResponseTemplate::new(201).set_body_json(json!({
                    "runner": {"id": "not-a-number"},
                    "encoded_jit_config": ENCODED_JIT_CONFIG,
                })),
            ))
            .await;

        let runner = generate(&github, &repository()).await.unwrap();
        let error = generate(&github, &repository()).await.unwrap_err();

        let output = format!("{runner:?}");
        assert!(!output.contains(ENCODED_JIT_CONFIG), "{output}");
        assert_matches!(error, RequestError::Decode { .. });
        fake::assert_secret_absent(&error, ENCODED_JIT_CONFIG);
    }
}
