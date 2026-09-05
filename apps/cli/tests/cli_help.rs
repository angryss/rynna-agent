use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn help_describes_interactive_run_and_server_modes() {
    let mut command = Command::cargo_bin("rynna").unwrap();
    command.arg("--help");

    command.assert().success().stdout(
        predicate::str::contains("An AI software agent")
            .and(predicate::str::contains("chat"))
            .and(predicate::str::contains("run"))
            .and(predicate::str::contains("serve"))
            .and(predicate::str::contains("profiles"))
            .and(predicate::str::contains("projects"))
            .and(predicate::str::contains("--profile"))
            .and(predicate::str::contains("--project"))
            .and(predicate::str::contains("--config"))
            .and(predicate::str::contains("--configure-providers"))
            .and(predicate::str::contains("--provider-config")),
    );
}

#[test]
fn provider_configuration_requires_an_interactive_terminal() {
    let mut command = Command::cargo_bin("rynna").unwrap();
    command.arg("--configure-providers");

    command
        .assert()
        .failure()
        .stderr(predicate::str::contains("requires an interactive terminal"));
}
