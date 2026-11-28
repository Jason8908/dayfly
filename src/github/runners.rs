use std::fmt;
use std::iter;

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use crate::config::{Label, PoolName, Scope};
use crate::instance;

use super::{Client, RequestError, runners_path};

const NAME_PREFIX: &str = "dayfly-";
const NAME_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
const NAME_SUFFIX_LEN: usize = 8;
const MAX_NAME_ATTEMPTS: u32 = 3;
const PAGE_SIZE: usize = 100;

#[derive(Debug, Clone, Copy)]
pub struct NewRunner<'a> {
    pub pool: &'a PoolName,
    pub instance: &'a instance::Id,
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
    pub(crate) fn generate(pool: &PoolName, instance: &instance::Id) -> Self {
        let suffix: String =
            iter::repeat_with(|| char::from(NAME_ALPHABET[fastrand::usize(..NAME_ALPHABET.len())]))
                .take(NAME_SUFFIX_LEN)
                .collect();
        Self(format!("{NAME_PREFIX}{pool}-{instance}-{suffix}"))
    }

    fn of_instance(name: String, instance: &instance::Id) -> Option<Self> {
        is_generated_for(&name, instance).then_some(Self(name))
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

fn is_generated_for(name: &str, instance: &instance::Id) -> bool {
    let Some((pool_and_instance, suffix)) = name
        .strip_prefix(NAME_PREFIX)
        .and_then(|rest| rest.rsplit_once('-'))
    else {
        return false;
    };
    let Some((pool, id)) = pool_and_instance.rsplit_once('-') else {
        return false;
    };
    id == instance.as_str()
        && suffix.len() == NAME_SUFFIX_LEN
        && suffix.bytes().all(|byte| NAME_ALPHABET.contains(&byte))
        && pool.parse::<PoolName>().is_ok()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Runner {
    id: RunnerId,
    name: RunnerName,
    status: RunnerStatus,
}

impl Runner {
    pub fn id(&self) -> RunnerId {
        self.id
    }

    pub fn name(&self) -> &RunnerName {
        &self.name
    }

    pub fn status(&self) -> RunnerStatus {
        self.status
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum RunnerStatus {
    Online,
    Offline,
    #[serde(other)]
    Unknown,
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

#[derive(Deserialize)]
struct RunnerPage {
    total_count: usize,
    runners: Vec<ListedRunner>,
}

#[derive(Deserialize)]
struct ListedRunner {
    id: RunnerId,
    name: String,
    status: RunnerStatus,
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
            let name = RunnerName::generate(runner.pool, runner.instance);
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

    pub async fn runners(
        &self,
        scope: &Scope,
        instance: &instance::Id,
    ) -> Result<Vec<Runner>, RequestError> {
        let path = runners_path(scope);
        let mut runners = Vec::new();
        let mut listed = 0;
        for page in 1.. {
            let response: RunnerPage = self
                .get(&format!("{path}?per_page={PAGE_SIZE}&page={page}"))
                .await?;
            listed += response.runners.len();
            let last_page = response.runners.is_empty() || listed >= response.total_count;
            runners.extend(response.runners.into_iter().filter_map(|runner| {
                Some(Runner {
                    id: runner.id,
                    name: RunnerName::of_instance(runner.name, instance)?,
                    status: runner.status,
                })
            }));
            if last_page {
                break;
            }
        }
        Ok(runners)
    }

    pub async fn delete_runner(&self, scope: &Scope, id: RunnerId) -> Result<(), RequestError> {
        match self.delete(&format!("{}/{id}", runners_path(scope))).await {
            Err(RequestError::NotFound(_)) => Ok(()),
            result => result,
        }
    }

    pub async fn delete_offline_runners(
        &self,
        scope: &Scope,
        instance: &instance::Id,
    ) -> Result<Vec<RunnerName>, RequestError> {
        let mut deleted = Vec::new();
        for runner in self.runners(scope, instance).await? {
            if runner.status == RunnerStatus::Offline {
                self.delete_runner(scope, runner.id).await?;
                deleted.push(runner.name);
            }
        }
        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use serde_json::{Value, json};
    use wiremock::matchers::{body_partial_json, method, query_param};
    use wiremock::{Mock, Request, ResponseTemplate};

    use super::super::fake;
    use super::*;

    const REPOSITORY_PATH: &str = "/repos/octo-org/octo-repo/actions/runners/generate-jitconfig";
    const ORGANIZATION_PATH: &str = "/orgs/octo-org/actions/runners/generate-jitconfig";
    const REPOSITORY_RUNNERS: &str = "/repos/octo-org/octo-repo/actions/runners";
    const ORGANIZATION_RUNNERS: &str = "/orgs/octo-org/actions/runners";
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

    fn instance() -> instance::Id {
        "k3j9x2ab".parse().unwrap()
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
            instance: &instance(),
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

    fn listed(id: u64, name: &str, status: &str) -> Value {
        json!({
            "id": id,
            "name": name,
            "os": "Linux",
            "status": status,
            "busy": false,
            "labels": [],
            "ephemeral": true,
        })
    }

    fn runner_page(total_count: usize, runners: &[Value]) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({
            "total_count": total_count,
            "runners": runners,
        }))
    }

    async fn mount_page(github: &fake::GitHub, path: &str, page: u32, response: ResponseTemplate) {
        github
            .mount(
                fake::request("GET", path)
                    .and(query_param("per_page", "100"))
                    .and(query_param("page", page.to_string()))
                    .respond_with(response)
                    .expect(1),
            )
            .await;
    }

    async fn deleted_paths(github: &fake::GitHub) -> Vec<String> {
        github
            .requests()
            .await
            .iter()
            .filter(|request| request.method.as_str() == "DELETE")
            .map(|request| request.url.path().to_owned())
            .collect()
    }

    fn names(runners: &[Runner]) -> Vec<&str> {
        runners
            .iter()
            .map(|runner| runner.name().as_str())
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
    fn runner_names_are_the_pool_instance_and_a_random_lowercase_suffix() {
        let name = RunnerName::generate(&pool(), &instance());

        let suffix = name
            .as_str()
            .strip_prefix("dayfly-linux-k3j9x2ab-")
            .unwrap();
        assert_eq!(suffix.len(), 8, "{name}");
        assert!(
            suffix
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit()),
            "{name}"
        );
        assert_ne!(RunnerName::generate(&pool(), &instance()), name);
    }

    #[test]
    fn generated_names_belong_only_to_their_instance() {
        let other: instance::Id = "zz99aa00".parse().unwrap();
        for pool in ["linux", "linux-arm64", "k3j9x2ab", "a-b-c"] {
            let name = RunnerName::generate(&pool.parse().unwrap(), &instance());

            assert_eq!(
                RunnerName::of_instance(name.to_string(), &instance()).as_ref(),
                Some(&name)
            );
            assert_eq!(RunnerName::of_instance(name.to_string(), &other), None);
        }
    }

    #[test]
    fn names_not_generated_for_this_instance_are_not_its_own() {
        for name in [
            "my-mac-runner",
            "dayfly-linux-7fh2k9qz",
            "dayfly-k3j9x2ab-7fh2k9qz",
            "dayfly--k3j9x2ab-7fh2k9qz",
            "dayfly-linux-zz99aa00-7fh2k9qz",
            "dayfly-linux-k3j9x2ab",
            "dayfly-linux-k3j9x2ab-7fh2k9q",
            "dayfly-linux-k3j9x2ab-7fh2k9qzz",
            "dayfly-linux-k3j9x2ab-7FH2K9QZ",
            "dayfly-linux-k3j9x2ab-7fh2k9qz-extra",
            "dayfly-Linux-k3j9x2ab-7fh2k9qz",
            "dayfly-linux_x64-k3j9x2ab-7fh2k9qz",
            "Dayfly-linux-k3j9x2ab-7fh2k9qz",
            "xdayfly-linux-k3j9x2ab-7fh2k9qz",
            "dayfly-linux-k3j9x2ab-7fh2k9qz ",
        ] {
            assert_eq!(
                RunnerName::of_instance(name.to_owned(), &instance()),
                None,
                "{name}"
            );
        }
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

    #[tokio::test]
    async fn runners_of_this_instance_are_listed_across_pages() {
        let github = fake::GitHub::start().await;
        mount_page(
            &github,
            REPOSITORY_RUNNERS,
            1,
            runner_page(
                3,
                &[
                    listed(1, "dayfly-linux-k3j9x2ab-7fh2k9qz", "online"),
                    listed(2, "my-mac-runner", "offline"),
                ],
            ),
        )
        .await;
        mount_page(
            &github,
            REPOSITORY_RUNNERS,
            2,
            runner_page(
                3,
                &[listed(3, "dayfly-linux-arm-k3j9x2ab-a1b2c3d4", "offline")],
            ),
        )
        .await;

        let runners = github
            .client()
            .runners(&repository(), &instance())
            .await
            .unwrap();

        assert_eq!(
            names(&runners),
            [
                "dayfly-linux-k3j9x2ab-7fh2k9qz",
                "dayfly-linux-arm-k3j9x2ab-a1b2c3d4"
            ]
        );
        assert_eq!(
            runners
                .iter()
                .map(|runner| (runner.id(), runner.status()))
                .collect::<Vec<_>>(),
            [
                (RunnerId(1), RunnerStatus::Online),
                (RunnerId(3), RunnerStatus::Offline)
            ]
        );
    }

    #[tokio::test]
    async fn listing_stops_at_an_empty_page() {
        let github = fake::GitHub::start().await;
        mount_page(&github, REPOSITORY_RUNNERS, 1, runner_page(5, &[])).await;

        let runners = github
            .client()
            .runners(&repository(), &instance())
            .await
            .unwrap();

        assert_eq!(runners, []);
    }

    #[tokio::test]
    async fn only_offline_runners_of_this_instance_are_deleted() {
        let github = fake::GitHub::start().await;
        mount_page(
            &github,
            REPOSITORY_RUNNERS,
            1,
            runner_page(
                9,
                &[
                    listed(1, "my-mac-runner", "offline"),
                    listed(2, "dayfly-linux-7fh2k9qz", "offline"),
                    listed(3, "dayfly-linux-zz99aa00-7fh2k9qz", "offline"),
                    listed(4, "dayfly-linux-k3j9x2ab-7fh2k9qz", "online"),
                    listed(5, "dayfly-linux-k3j9x2ab-a1b2c3d4", "offline"),
                ],
            ),
        )
        .await;
        mount_page(
            &github,
            REPOSITORY_RUNNERS,
            2,
            runner_page(
                9,
                &[
                    listed(6, "dayfly-linux-k3j9x2ab-0000aaaa", "starting"),
                    listed(7, "dayfly-linux-k3j9x2ab", "offline"),
                    listed(8, "xdayfly-linux-k3j9x2ab-b2c3d4e5", "offline"),
                    listed(9, "dayfly-gpu-k3j9x2ab-c3d4e5f6", "offline"),
                ],
            ),
        )
        .await;
        github
            .mount(Mock::given(method("DELETE")).respond_with(ResponseTemplate::new(204)))
            .await;

        let deleted = github
            .client()
            .delete_offline_runners(&repository(), &instance())
            .await
            .unwrap();

        assert_eq!(
            deleted_paths(&github).await,
            [
                format!("{REPOSITORY_RUNNERS}/5"),
                format!("{REPOSITORY_RUNNERS}/9")
            ]
        );
        assert_eq!(
            deleted.iter().map(RunnerName::as_str).collect::<Vec<_>>(),
            [
                "dayfly-linux-k3j9x2ab-a1b2c3d4",
                "dayfly-gpu-k3j9x2ab-c3d4e5f6"
            ]
        );
    }

    #[tokio::test]
    async fn organization_runners_are_listed_and_deleted() {
        let github = fake::GitHub::start().await;
        mount_page(
            &github,
            ORGANIZATION_RUNNERS,
            1,
            runner_page(1, &[listed(7, "dayfly-linux-k3j9x2ab-7fh2k9qz", "offline")]),
        )
        .await;
        github
            .mount(
                fake::request("DELETE", &format!("{ORGANIZATION_RUNNERS}/7"))
                    .respond_with(ResponseTemplate::new(204))
                    .expect(1),
            )
            .await;

        let deleted = github
            .client()
            .delete_offline_runners(&organization(), &instance())
            .await
            .unwrap();

        assert_eq!(
            deleted.iter().map(RunnerName::as_str).collect::<Vec<_>>(),
            ["dayfly-linux-k3j9x2ab-7fh2k9qz"]
        );
    }

    #[tokio::test]
    async fn deleting_a_runner_that_is_already_gone_succeeds() {
        let github = fake::GitHub::start().await;
        github
            .mount(
                fake::request("DELETE", &format!("{REPOSITORY_RUNNERS}/7"))
                    .respond_with(fake::error(404, "Not Found"))
                    .expect(1),
            )
            .await;

        github
            .client()
            .delete_runner(&repository(), RunnerId(7))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn other_delete_failures_are_returned() {
        let github = fake::GitHub::start().await;
        github
            .mount(
                fake::request("DELETE", &format!("{REPOSITORY_RUNNERS}/7"))
                    .respond_with(fake::error(422, "Validation Failed"))
                    .expect(1),
            )
            .await;

        assert_matches!(
            github
                .client()
                .delete_runner(&repository(), RunnerId(7))
                .await,
            Err(RequestError::Unprocessable(_))
        );
    }
}
