use std::fmt;

use reqwest::Method;

use crate::config::Scope;

use super::{Client, Endpoint, ErrorResponse, RequestError, runners_path};

impl Client {
    pub async fn check_access<'a>(
        &self,
        scopes: impl IntoIterator<Item = &'a Scope>,
    ) -> Result<(), AccessError> {
        let mut checked: Vec<&Scope> = Vec::new();
        let mut denials = Vec::new();
        for scope in scopes {
            if checked.contains(&scope) {
                continue;
            }
            checked.push(scope);
            let path = format!("{}/registration-token", runners_path(scope));
            let denial = |refusal, response| Denial {
                scope: scope.clone(),
                refusal,
                response,
            };
            match self.send(&Endpoint::new(Method::POST, &path), None).await {
                Ok(_) => tracing::debug!(%scope, "token can register runners"),
                Err(RequestError::Unauthorized(response)) => {
                    return Err(AccessError::Unauthorized(response));
                }
                Err(RequestError::Forbidden(response)) => {
                    denials.push(denial(Refusal::Forbidden, response));
                }
                Err(RequestError::NotFound(response)) => {
                    denials.push(denial(Refusal::NotFound, response));
                }
                Err(source) => {
                    return Err(AccessError::Request {
                        scope: scope.clone(),
                        source,
                    });
                }
            }
        }
        if denials.is_empty() {
            Ok(())
        } else {
            Err(AccessError::Denied(denials))
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AccessError {
    #[error("the token is invalid, expired or revoked: {0}")]
    Unauthorized(Box<ErrorResponse>),
    #[error("the token cannot register runners:{}", denial_lines(.0))]
    Denied(Vec<Denial>),
    #[error("failed to check the token's access to {scope}")]
    Request {
        scope: Scope,
        #[source]
        source: RequestError,
    },
}

fn denial_lines(denials: &[Denial]) -> impl fmt::Display + '_ {
    fmt::from_fn(move |f| {
        for denial in denials {
            write!(f, "\n  {denial}")?;
        }
        Ok(())
    })
}

#[derive(Debug)]
pub struct Denial {
    scope: Scope,
    refusal: Refusal,
    response: Box<ErrorResponse>,
}

#[derive(Debug, Clone, Copy)]
enum Refusal {
    Forbidden,
    NotFound,
}

impl fmt::Display for Denial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            scope,
            refusal,
            response,
        } = self;
        match refusal {
            Refusal::Forbidden => write!(
                f,
                "{scope}: the token needs {} set to \"Read and write\" ({response})",
                required_permission(scope)
            ),
            Refusal::NotFound => write!(
                f,
                "{scope}: not found, or the token was not granted access to it ({response})"
            ),
        }
    }
}

fn required_permission(scope: &Scope) -> &'static str {
    match scope {
        Scope::Repository(_) => "the \"Administration\" repository permission",
        Scope::Organization(_) => "the \"Self-hosted runners\" organization permission",
    }
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use serde_json::json;
    use wiremock::ResponseTemplate;

    use super::super::fake;
    use super::*;

    const REPOSITORY_PATH: &str = "/repos/octo-org/octo-repo/actions/runners/registration-token";
    const ORGANIZATION_PATH: &str = "/orgs/octo-org/actions/runners/registration-token";

    fn repository() -> Scope {
        Scope::Repository("octo-org/octo-repo".parse().unwrap())
    }

    fn organization() -> Scope {
        Scope::Organization("octo-org".parse().unwrap())
    }

    fn registration_token() -> ResponseTemplate {
        ResponseTemplate::new(201).set_body_json(json!({
            "token": "LLBF3JGZDX3P5PMEXLND6TS6FCWO6",
            "expires_at": "2026-11-25T13:00:00.000Z",
        }))
    }

    async fn github(responses: Vec<(&str, ResponseTemplate, u64)>) -> fake::GitHub {
        let github = fake::GitHub::start().await;
        for (path, response, calls) in responses {
            github
                .mount(
                    fake::request("POST", path)
                        .respond_with(response)
                        .expect(calls),
                )
                .await;
        }
        github
    }

    async fn failing_check(response: ResponseTemplate) -> AccessError {
        let github = github(vec![(REPOSITORY_PATH, response, 1)]).await;
        github
            .client()
            .check_access(&[repository()])
            .await
            .unwrap_err()
    }

    #[tokio::test]
    async fn each_repository_and_organization_is_checked_once() {
        let github = github(vec![
            (REPOSITORY_PATH, registration_token(), 1),
            (ORGANIZATION_PATH, registration_token(), 1),
        ])
        .await;

        github
            .client()
            .check_access(&[repository(), organization(), repository()])
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn missing_repository_permission_is_named() {
        let error = failing_check(fake::error(
            403,
            "Resource not accessible by personal access token",
        ))
        .await;

        assert_eq!(
            error.to_string(),
            format!(
                "the token cannot register runners:\n  repository octo-org/octo-repo: the token \
                 needs the \"Administration\" repository permission set to \"Read and write\" \
                 (POST {REPOSITORY_PATH} returned 403 Forbidden: Resource not accessible by \
                 personal access token)"
            )
        );
    }

    #[tokio::test]
    async fn every_denied_scope_is_reported() {
        let other_repository = Scope::Repository("octo-org/other".parse().unwrap());
        let github = github(vec![
            (REPOSITORY_PATH, fake::error(404, "Not Found"), 1),
            (
                ORGANIZATION_PATH,
                fake::error(403, "Resource not accessible by personal access token"),
                1,
            ),
            (
                "/repos/octo-org/other/actions/runners/registration-token",
                registration_token(),
                1,
            ),
        ])
        .await;

        let error = github
            .client()
            .check_access(&[repository(), organization(), other_repository])
            .await
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            format!(
                "the token cannot register runners:\n  repository octo-org/octo-repo: not found, \
                 or the token was not granted access to it (POST {REPOSITORY_PATH} returned 404 \
                 Not Found: Not Found)\n  organization octo-org: the token needs the \
                 \"Self-hosted runners\" organization permission set to \"Read and write\" (POST \
                 {ORGANIZATION_PATH} returned 403 Forbidden: Resource not accessible by personal \
                 access token)"
            )
        );
    }

    #[tokio::test]
    async fn rejected_token_stops_the_check() {
        let github = github(vec![
            (REPOSITORY_PATH, fake::error(401, "Bad credentials"), 1),
            (ORGANIZATION_PATH, registration_token(), 0),
        ])
        .await;

        let error = github
            .client()
            .check_access(&[repository(), organization()])
            .await
            .unwrap_err();

        assert_matches!(error, AccessError::Unauthorized(_));
        assert_eq!(
            error.to_string(),
            format!(
                "the token is invalid, expired or revoked: POST {REPOSITORY_PATH} returned 401 \
                 Unauthorized: Bad credentials"
            )
        );
    }

    #[tokio::test]
    async fn other_failures_name_the_scope_and_are_not_retried() {
        let error = failing_check(fake::error(500, "Server Error")).await;

        assert_eq!(
            error.to_string(),
            "failed to check the token's access to repository octo-org/octo-repo"
        );
        assert_matches!(
            error,
            AccessError::Request {
                source: RequestError::Unexpected(_),
                ..
            }
        );
    }

    #[tokio::test]
    async fn no_access_error_contains_the_token() {
        for status in [401, 403, 404, 500] {
            fake::assert_no_token(&failing_check(fake::error(status, "message")).await);
        }
    }
}
