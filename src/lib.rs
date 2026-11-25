use std::io::{self, Write};

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RunError {
    #[error("failed to write output")]
    Output(#[source] io::Error),
}

pub fn run(mut output: impl Write) -> Result<(), RunError> {
    writeln!(output, "dayfly {}", env!("CARGO_PKG_VERSION")).map_err(RunError::Output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prints_the_version() {
        let mut output = Vec::new();

        run(&mut output).unwrap();

        assert_eq!(
            String::from_utf8(output).unwrap(),
            format!("dayfly {}\n", env!("CARGO_PKG_VERSION"))
        );
    }
}
