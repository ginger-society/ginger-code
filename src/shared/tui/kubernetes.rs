


pub async fn shell_into_pod(deployment_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    use std::process::Stdio;

    let pod_output = tokio::process::Command::new("kubectl")
        .args(&[
            "get",
            "pods",
            "-l",
            &format!("app={}", deployment_name),
            "--no-headers",
            "-o",
            "custom-columns=NAME:.metadata.name",
        ])
        .output()
        .await?;

    let pod_name = String::from_utf8_lossy(&pod_output.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .next()
        .map(|l| l.trim().to_string());

    let Some(pod) = pod_name else {
        eprintln!("No running pod found for deployment: {}", deployment_name);
        return Ok(());
    };

    // Try bash first, fall back to sh
    let bash_ok = tokio::process::Command::new("kubectl")
        .args(["exec", "-it", &pod, "--", "bash"])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false);

    if !bash_ok {
        tokio::process::Command::new("kubectl")
            .args(["exec", "-it", &pod, "--", "sh"])
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .await?;
    }

    Ok(())
}