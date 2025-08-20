use assert_cmd::Command;

fn network_available() -> bool {
    let config = ureq::Agent::config_builder()
        .timeout_connect(Some(std::time::Duration::from_secs(2)))
        .timeout_global(Some(std::time::Duration::from_secs(5)))
        .build();
    let agent = ureq::Agent::new_with_config(config);
    agent
        .get("https://doi.org/")
        .call()
        .map(|res| !res.status().is_server_error())
        .unwrap_or(false)
}

#[test]
fn fetch_simple_webpage() -> Result<(), Box<dyn std::error::Error>> {
    if !network_available() {
        eprintln!("skipping fetch_simple_webpage: network unavailable");
        return Ok(());
    }
    let mut cmd = Command::cargo_bin("bib")?;
    cmd.env("NO_COLOR", "1");
    let url = "https://example.com/";
    let output = cmd.arg("fetch").arg(url).output()?;
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8(strip_ansi_escapes::strip(output.stderr))?;
    assert!(
        stdout.contains("@online") && stdout.contains("Example Domain"),
        "stdout did not contain expected @online with Example Domain. stdout=\n{}",
        stdout
    );
    assert!(
        stderr.contains("✓ 1") && stderr.contains("✗ 0"),
        "stderr summary mismatch. stderr=\n{}",
        stderr
    );
    Ok(())
}

