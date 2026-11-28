#[cfg(test)]
pub mod fake;

use crate::config::{Image, Limits, PoolName};
use crate::github::{JitConfig, RunnerName};

#[derive(Debug)]
#[non_exhaustive]
pub enum Backend {
    #[cfg(test)]
    Fake(fake::Fake),
}

#[cfg_attr(
    test,
    expect(
        clippy::unused_async,
        clippy::unused_async_trait_impl,
        reason = "the in-memory test fake is the only backend"
    )
)]
#[cfg_attr(
    not(test),
    expect(
        clippy::unused_async,
        unused_variables,
        reason = "the in-memory test fake is the only backend"
    )
)]
impl Backend {
    pub async fn provision(&self, spec: &RunnerSpec<'_>) -> Result<Handle, Error> {
        match *self {
            #[cfg(test)]
            Self::Fake(ref fake) => fake.provision(spec).await,
        }
    }

    pub async fn wait(&self, handle: &Handle) -> Result<Exit, Error> {
        match *self {
            #[cfg(test)]
            Self::Fake(ref fake) => fake.wait(handle).await,
        }
    }

    /// Succeeds when the environment is already gone.
    pub async fn destroy(&self, handle: &Handle) -> Result<(), Error> {
        match *self {
            #[cfg(test)]
            Self::Fake(ref fake) => {
                fake.destroy(handle);
                Ok(())
            }
        }
    }

    /// Lists this dayfly instance's environments, including those whose runner has exited.
    pub async fn list(&self) -> Result<Vec<Handle>, Error> {
        match *self {
            #[cfg(test)]
            Self::Fake(ref fake) => Ok(fake.list()),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RunnerSpec<'a> {
    pub pool: &'a PoolName,
    pub runner: &'a RunnerName,
    pub image: &'a Image,
    pub limits: Limits,
    pub jit_config: &'a JitConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Handle {
    pool: PoolName,
    runner: RunnerName,
}

impl Handle {
    pub fn pool(&self) -> &PoolName {
        &self.pool
    }

    pub fn runner(&self) -> &RunnerName {
        &self.runner
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exit {
    code: i64,
}

impl Exit {
    pub fn code(self) -> i64 {
        self.code
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("the environment of runner `{0}` does not exist")]
    NotFound(RunnerName),
    #[cfg(test)]
    #[error("the fake backend was told to fail")]
    Fake,
}
