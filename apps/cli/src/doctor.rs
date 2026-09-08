//! `rynna doctor`: validate configuration without contacting a model provider.
//!
//! Configuration errors are otherwise found either at startup or, worse, part-way
//! through an unattended run. This checks the things that make a profile fail to
//! work — missing credential variables, capability paths that no longer exist,
//! skills that cannot be read — and reports all of them at once.
//!
//! It performs no network I/O and never reads or prints a credential value; it
//! only reports whether the named environment variable is set.

use std::path::Path;

use anyhow::Result;
use rynna_config::{ProfileCatalog, ProviderKind, ResolvedCapability};
use serde::Serialize;

use crate::OutputFormat;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Ok,
    /// The profile still loads, but something will not behave as configured.
    Warning,
    /// The profile cannot work as written.
    Failure,
}

impl Severity {
    fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warning => "warn",
            Self::Failure => "fail",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Finding {
    pub profile: String,
    pub check: &'static str,
    pub severity: Severity,
    pub detail: String,
}

#[derive(Debug, Serialize)]
struct Report {
    default_profile: String,
    findings: Vec<Finding>,
}

pub fn run(catalog: &ProfileCatalog, default_profile: &str, output: OutputFormat) -> Result<()> {
    let mut findings = Vec::new();

    match catalog.resolve(default_profile) {
        Ok(_) => {}
        Err(error) => findings.push(Finding {
            profile: default_profile.to_owned(),
            check: "default-profile",
            severity: Severity::Failure,
            detail: format!("the default profile does not resolve: {error}"),
        }),
    }

    match catalog.resolve_all() {
        Ok(profiles) => {
            for resolved in profiles {
                let name = resolved.profile.name.clone();
                check_providers(&name, &resolved, &mut findings);
                check_capabilities(&name, &resolved, &mut findings);
                check_skills(&name, &resolved, &mut findings);
            }
        }
        Err(error) => findings.push(Finding {
            profile: "-".to_owned(),
            check: "catalog",
            severity: Severity::Failure,
            detail: format!("the catalog does not resolve: {error}"),
        }),
    }

    let worst = findings
        .iter()
        .map(|f| f.severity)
        .max()
        .unwrap_or(Severity::Ok);
    emit(default_profile, findings, output)?;

    // A non-zero exit lets `rynna doctor` gate a deploy or a cron wrapper.
    if worst == Severity::Failure {
        std::process::exit(1);
    }
    Ok(())
}

fn check_providers(
    profile: &str,
    resolved: &rynna_config::ResolvedProfile,
    findings: &mut Vec<Finding>,
) {
    if resolved.providers.is_empty() {
        findings.push(Finding {
            profile: profile.to_owned(),
            check: "provider",
            severity: Severity::Failure,
            detail: "no enabled provider".to_owned(),
        });
        return;
    }
    for provider in &resolved.providers {
        match &provider.api_key_env {
            Some(variable) if std::env::var_os(variable).is_none() => findings.push(Finding {
                profile: profile.to_owned(),
                check: "credentials",
                severity: Severity::Failure,
                // Report only that the variable is unset, never any value.
                detail: format!(
                    "provider `{}` needs ${variable}, which is not set",
                    provider.name
                ),
            }),
            Some(variable) => findings.push(Finding {
                profile: profile.to_owned(),
                check: "credentials",
                severity: Severity::Ok,
                detail: format!("provider `{}` reads ${variable}", provider.name),
            }),
            None if provider.provider_kind == ProviderKind::ClaudeSubscription => {
                let program = &provider.claude_program;
                if program.is_absolute() {
                    findings.push(if program.exists() {
                        Finding {
                            profile: profile.to_owned(),
                            check: "claude-program",
                            severity: Severity::Ok,
                            detail: format!("{} exists", program.display()),
                        }
                    } else {
                        Finding {
                            profile: profile.to_owned(),
                            check: "claude-program",
                            severity: Severity::Failure,
                            detail: format!("{} does not exist", program.display()),
                        }
                    });
                } else if !on_path(program) {
                    // Resolution happens in the child's environment, which may differ.
                    findings.push(Finding {
                        profile: profile.to_owned(),
                        check: "claude-program",
                        severity: Severity::Warning,
                        detail: format!(
                            "`{}` was not found on this PATH; it must resolve when Rynna runs",
                            program.display()
                        ),
                    });
                }
            }
            None => findings.push(Finding {
                profile: profile.to_owned(),
                check: "credentials",
                severity: Severity::Ok,
                detail: format!("provider `{}` needs no credentials", provider.name),
            }),
        }
    }
}

fn check_capabilities(
    profile: &str,
    resolved: &rynna_config::ResolvedProfile,
    findings: &mut Vec<Finding>,
) {
    for capability in &resolved.capabilities {
        match capability {
            ResolvedCapability::FileSystem(filesystem) => {
                report_directory(profile, "filesystem-root", &filesystem.root, findings);
            }
            ResolvedCapability::Command(command) => {
                report_directory(
                    profile,
                    "command-working-directory",
                    &command.working_directory,
                    findings,
                );
                for (alias, path) in &command.programs {
                    if path.exists() && is_executable(path) {
                        findings.push(Finding {
                            profile: profile.to_owned(),
                            check: "command-program",
                            severity: Severity::Ok,
                            detail: format!("`{alias}` maps to {}", path.display()),
                        });
                    } else if !path.exists() {
                        findings.push(Finding {
                            profile: profile.to_owned(),
                            check: "command-program",
                            severity: Severity::Failure,
                            detail: format!(
                                "`{alias}` maps to {}, which does not exist",
                                path.display()
                            ),
                        });
                    } else {
                        findings.push(Finding {
                            profile: profile.to_owned(),
                            check: "command-program",
                            severity: Severity::Failure,
                            detail: format!(
                                "`{alias}` maps to {}, which is not executable",
                                path.display()
                            ),
                        });
                    }
                }
            }
        }
    }
}

fn check_skills(
    profile: &str,
    resolved: &rynna_config::ResolvedProfile,
    findings: &mut Vec<Finding>,
) {
    for skill in &resolved.profile.active_skills {
        let manifest = resolved.skills_directory.join(skill).join("SKILL.md");
        findings.push(if manifest.exists() {
            Finding {
                profile: profile.to_owned(),
                check: "skill",
                severity: Severity::Ok,
                detail: format!("skill `{skill}` is readable"),
            }
        } else {
            Finding {
                profile: profile.to_owned(),
                check: "skill",
                severity: Severity::Failure,
                detail: format!("skill `{skill}` has no {}", manifest.display()),
            }
        });
    }
}

/// Returns true when a bare program name resolves on the current PATH.
fn on_path(program: &Path) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|directory| {
        let candidate = directory.join(program);
        candidate.is_file() && is_executable(&candidate)
    })
}

fn report_directory(profile: &str, check: &'static str, path: &Path, findings: &mut Vec<Finding>) {
    if path.is_dir() {
        findings.push(Finding {
            profile: profile.to_owned(),
            check,
            severity: Severity::Ok,
            detail: format!("{} exists", path.display()),
        });
        return;
    }
    if !path.exists() {
        findings.push(Finding {
            profile: profile.to_owned(),
            check,
            severity: Severity::Failure,
            detail: format!("{} does not exist", path.display()),
        });
    } else if !path.is_dir() {
        findings.push(Finding {
            profile: profile.to_owned(),
            check,
            severity: Severity::Failure,
            detail: format!("{} is not a directory", path.display()),
        });
    }
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|meta| meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    true
}

fn emit(default_profile: &str, findings: Vec<Finding>, output: OutputFormat) -> Result<()> {
    match output {
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string(&Report {
                default_profile: default_profile.to_owned(),
                findings,
            })?
        ),
        OutputFormat::Text => {
            let problems = findings
                .iter()
                .filter(|finding| finding.severity != Severity::Ok)
                .count();
            for finding in &findings {
                println!(
                    "{:<5} {:<26} [{}] {}",
                    finding.severity.label(),
                    finding.check,
                    finding.profile,
                    finding.detail
                );
            }
            println!();
            let total = findings.len();
            let checks = if total == 1 { "check" } else { "checks" };
            if problems == 0 {
                println!("No problems found across {total} {checks}.");
            } else {
                println!("{problems} of {total} {checks} need attention.");
            }
        }
    }
    Ok(())
}
