use assert_cmd::Command;
fn network_available() -> bool {
    let config = ureq::Agent::config_builder()
        .timeout_connect(Some(std::time::Duration::from_secs(2)))
        .timeout_global(Some(std::time::Duration::from_secs(5)))
        .build();
    let agent = ureq::Agent::new_with_config(config);
    agent.get("https://doi.org/")
        .call()
        .map(|res| !res.status().is_server_error())
        .unwrap_or(false)
}

#[test]
fn fetch_single_valid_doi() -> Result<(), Box<dyn std::error::Error>> {
    let mut cmd = Command::cargo_bin("bib")?;
    cmd.env("NO_COLOR", "1");

    if !network_available() {
        eprintln!("skipping fetch_single_valid_doi: network unavailable");
        return Ok(());
    }

    // Known, stable DOI: "DOI of the DOI System" (DOI Handbook)
    let doi = "10.1000/182";

    let output = cmd.arg("fetch").arg(doi).output()?;
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8(strip_ansi_escapes::strip(output.stderr))?;
    assert!(stdout.contains("10.1000/182"), "stdout did not contain expected DOI. stdout=\n{}", stdout);
    assert!(stderr.contains("✓ 1") && stderr.contains("✗ 0"), "stderr summary mismatch. stderr=\n{}", stderr);

    Ok(())
}

#[test]
fn fetch_non_existent_doi() -> Result<(), Box<dyn std::error::Error>> {
    let mut cmd = Command::cargo_bin("bib")?;
    cmd.env("NO_COLOR", "1");

    if !network_available() {
        eprintln!("skipping fetch_non_existent_doi: network unavailable");
        return Ok(());
    }

    // Syntactically valid but non-existent DOI (unlikely to ever exist)
    let doi = "10.9999/this-doi-does-not-exist";

    let output = cmd.arg("fetch").arg(doi).output()?;
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8(strip_ansi_escapes::strip(output.stderr))?;
    assert!(stdout.is_empty(), "stdout should be empty for non-existent DOI, got=\n{}", stdout);
    assert!(stderr.contains("✓ 0") && stderr.contains("✗ 1"), "stderr summary mismatch. stderr=\n{}", stderr);

    Ok(())
}

#[test]
fn fetch_invalid_identifier() -> Result<(), Box<dyn std::error::Error>> {
    let mut cmd = Command::cargo_bin("bib")?;
    cmd.env("NO_COLOR", "1");

    if !network_available() {
        eprintln!("skipping fetch_invalid_identifier: network unavailable");
        return Ok(());
    }

    let identifier = "this-is-not-a-doi";

    let output = cmd.arg("fetch").arg(identifier).output()?;
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8(strip_ansi_escapes::strip(output.stderr))?;
    assert!(stdout.is_empty(), "stdout should be empty for invalid identifier, got=\n{}", stdout);
    assert!(
        stderr.contains("unrecognised identifier: this-is-not-a-doi")
            && stderr.contains("✓ 0")
            && stderr.contains("✗ 1"),
        "stderr mismatch. stderr=\n{}",
        stderr
    );

    Ok(())
}
