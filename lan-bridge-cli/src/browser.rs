use std::{io, process::Stdio};
use tokio::process::{Child, Command};

fn spawn_chrome(url: &str, candidates: &[&str]) -> io::Result<Child> {
    for candidate in candidates {
        match Command::new(candidate)
            .arg(url)
            .stdin(Stdio::null())
            // Browser diagnostics can echo the URL/access key.
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => return Ok(child),
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, "未找到 Chrome"))
}

pub async fn open_chrome(url: String) {
    let mut child = match spawn_chrome(&url, &["google-chrome-stable", "google-chrome"]) {
        Ok(child) => child,
        Err(error) => {
            tracing::warn!(kind = ?error.kind(), "browser_launch_failed");
            return;
        }
    };
    tracing::info!("browser_launch_requested");
    // Wait off the server's main path to reap the launcher. A newly started
    // browser may stay alive for the entire session; it must not delay serving.
    match child.wait().await {
        Ok(status) if !status.success() => {
            tracing::warn!(code = ?status.code(), "browser_launch_failed");
        }
        Err(error) => tracing::warn!(kind = ?error.kind(), "browser_wait_failed"),
        _ => {}
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn falls_back_to_available_chrome_and_passes_url_as_one_argument() {
        let state = crate::tests::TestState::new().await;
        let launcher = state.0.dir.join("fake-chrome");
        let arguments = state.0.dir.join("arguments");
        tokio::fs::write(
            &launcher,
            format!(
                "#!/bin/sh\nprintf '%s' \"$1\" > '{}'\n\
                 test \"$#\" -eq 1\n",
                arguments.display()
            ),
        )
        .await
        .unwrap();
        tokio::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755))
            .await
            .unwrap();
        let missing = state.0.dir.join("missing");
        let url = "http://192.168.1.2:3000/?key=private&not-a-command=hello";
        let mut child = spawn_chrome(
            url,
            &[missing.to_str().unwrap(), launcher.to_str().unwrap()],
        )
        .unwrap();
        assert!(child.wait().await.unwrap().success());
        assert_eq!(tokio::fs::read_to_string(arguments).await.unwrap(), url);
    }

    #[tokio::test]
    async fn missing_chrome_returns_a_recoverable_error() {
        let state = crate::tests::TestState::new().await;
        let missing = state.0.dir.join("missing");
        let error = spawn_chrome("http://localhost/", &[missing.to_str().unwrap()])
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }
}
