use std::collections::VecDeque;
use std::future;
use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::watch;

use super::{Error, Exit, Handle, RunnerSpec};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provision {
    Succeed,
    Fail,
    Hang,
}

#[derive(Debug, Clone, Default)]
pub struct Fake {
    state: Arc<Mutex<State>>,
}

#[derive(Debug, Default)]
struct State {
    provisions: VecDeque<Provision>,
    environments: Vec<Environment>,
}

#[derive(Debug)]
struct Environment {
    handle: Handle,
    exit: watch::Sender<Option<Exit>>,
}

impl Fake {
    pub fn script_provisions(&self, provisions: impl IntoIterator<Item = Provision>) {
        self.state().provisions.extend(provisions);
    }

    pub fn exit(&self, handle: &Handle, code: i64) {
        self.state()
            .environment(handle)
            .unwrap_or_else(|| panic!("runner `{}` has no environment", handle.runner()))
            .exit
            .send_replace(Some(Exit { code }));
    }

    pub(super) async fn provision(&self, spec: &RunnerSpec<'_>) -> Result<Handle, Error> {
        let provision = self
            .state()
            .provisions
            .pop_front()
            .unwrap_or(Provision::Succeed);
        match provision {
            Provision::Succeed => {
                let handle = Handle {
                    pool: spec.pool.clone(),
                    runner: spec.runner.clone(),
                };
                self.state().environments.push(Environment {
                    handle: handle.clone(),
                    exit: watch::Sender::new(None),
                });
                Ok(handle)
            }
            Provision::Fail => Err(Error::Fake),
            Provision::Hang => future::pending().await,
        }
    }

    pub(super) async fn wait(&self, handle: &Handle) -> Result<Exit, Error> {
        let not_found = || Error::NotFound(handle.runner().clone());
        let mut exit = self
            .state()
            .environment(handle)
            .ok_or_else(not_found)?
            .exit
            .subscribe();
        exit.wait_for(Option::is_some)
            .await
            .ok()
            .and_then(|exit| *exit)
            .ok_or_else(not_found)
    }

    pub(super) fn destroy(&self, handle: &Handle) {
        self.state()
            .environments
            .retain(|environment| environment.handle != *handle);
    }

    pub(super) fn list(&self) -> Vec<Handle> {
        self.state()
            .environments
            .iter()
            .map(|environment| environment.handle.clone())
            .collect()
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }
}

impl State {
    fn environment(&self, handle: &Handle) -> Option<&Environment> {
        self.environments
            .iter()
            .find(|environment| environment.handle == *handle)
    }
}

#[cfg(test)]
mod tests {
    use std::assert_matches;
    use std::fmt::Debug;
    use std::time::Duration;

    use serde_json::json;
    use tokio::time;

    use super::super::Backend;
    use super::*;
    use crate::config::{Image, Limits, PoolName};
    use crate::github::{JitConfig, RunnerName};

    const JIT_CONFIG: &str = "eyJkYXlmbHkiOiJmYWtlIEpJVCBjb25maWcifQ==";
    const LATER: Duration = Duration::from_mins(1);
    const TIMEOUT: Duration = Duration::from_hours(1);

    struct Runner {
        pool: PoolName,
        name: RunnerName,
        image: Image,
        jit_config: JitConfig,
    }

    impl Runner {
        fn new(pool: &str) -> Self {
            let pool = pool.parse().unwrap();
            Self {
                name: RunnerName::generate(&pool, &"k3j9x2ab".parse().unwrap()),
                pool,
                image: "runner:latest".parse().unwrap(),
                jit_config: serde_json::from_value(json!(JIT_CONFIG)).unwrap(),
            }
        }

        fn spec(&self) -> RunnerSpec<'_> {
            RunnerSpec {
                pool: &self.pool,
                runner: &self.name,
                image: &self.image,
                limits: Limits::default(),
                jit_config: &self.jit_config,
            }
        }
    }

    fn backend() -> (Fake, Backend) {
        let fake = Fake::default();
        (fake.clone(), Backend::Fake(fake))
    }

    async fn provision(backend: &Backend, pool: &str) -> Result<Handle, Error> {
        backend.provision(&Runner::new(pool).spec()).await
    }

    async fn assert_never_finishes<T: Debug>(future: impl Future<Output = T>) {
        time::timeout(TIMEOUT, future).await.unwrap_err();
    }

    async fn finished<T>(future: impl Future<Output = T>) -> T {
        time::timeout(TIMEOUT, future).await.unwrap()
    }

    #[tokio::test]
    async fn provisioned_runners_are_listed_until_destroyed() {
        let (_, backend) = backend();
        let runner = Runner::new("linux");

        let linux = backend.provision(&runner.spec()).await.unwrap();
        let gpu = provision(&backend, "gpu").await.unwrap();

        assert_eq!(linux.pool(), &runner.pool);
        assert_eq!(linux.runner(), &runner.name);
        assert_eq!(backend.list().await.unwrap(), [linux.clone(), gpu.clone()]);
        backend.destroy(&linux).await.unwrap();
        assert_eq!(backend.list().await.unwrap(), [gpu]);
    }

    #[tokio::test]
    async fn destroying_a_destroyed_runner_succeeds() {
        let (_, backend) = backend();
        let handle = provision(&backend, "linux").await.unwrap();

        backend.destroy(&handle).await.unwrap();
        backend.destroy(&handle).await.unwrap();
    }

    #[tokio::test]
    async fn scripted_provisions_fail_and_later_ones_succeed() {
        let (fake, backend) = backend();
        fake.script_provisions([Provision::Fail, Provision::Succeed, Provision::Fail]);

        assert_matches!(provision(&backend, "linux").await, Err(Error::Fake));
        let second = provision(&backend, "linux").await.unwrap();
        assert_matches!(provision(&backend, "linux").await, Err(Error::Fake));
        let fourth = provision(&backend, "linux").await.unwrap();

        assert_eq!(backend.list().await.unwrap(), [second, fourth]);
    }

    #[tokio::test(start_paused = true)]
    async fn hanging_provision_never_finishes_or_creates_an_environment() {
        let (fake, backend) = backend();
        fake.script_provisions([Provision::Hang]);

        assert_never_finishes(provision(&backend, "linux")).await;

        assert_eq!(backend.list().await.unwrap(), []);
        provision(&backend, "linux").await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn wait_returns_once_the_runner_exits() {
        let (fake, backend) = backend();
        let handle = provision(&backend, "linux").await.unwrap();

        assert_never_finishes(backend.wait(&handle)).await;
        let (exit, ()) = finished(async {
            tokio::join!(backend.wait(&handle), async {
                time::sleep(LATER).await;
                fake.exit(&handle, 1);
            })
        })
        .await;

        assert_eq!(exit.unwrap().code(), 1);
        assert_eq!(finished(backend.wait(&handle)).await.unwrap().code(), 1);
        assert_eq!(backend.list().await.unwrap(), [handle]);
    }

    #[tokio::test(start_paused = true)]
    async fn destroying_a_runner_ends_its_wait_with_not_found() {
        let (_, backend) = backend();
        let handle = provision(&backend, "linux").await.unwrap();

        let (exit, destroyed) = finished(async {
            tokio::join!(backend.wait(&handle), async {
                time::sleep(LATER).await;
                backend.destroy(&handle).await
            })
        })
        .await;

        destroyed.unwrap();
        assert_matches!(exit, Err(Error::NotFound(runner)) if &runner == handle.runner());
        assert_matches!(
            finished(backend.wait(&handle)).await,
            Err(Error::NotFound(_))
        );
    }

    #[test]
    fn spec_debug_output_hides_the_jit_config() {
        let output = format!("{:?}", Runner::new("linux").spec());

        assert!(output.contains("runner:latest"), "{output}");
        assert!(!output.contains(JIT_CONFIG), "{output}");
    }
}
