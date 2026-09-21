//! Read-only host metadata. No shell, environment expansion or caller-selected files.
use async_trait::async_trait;
use rynna_core::{Tool, ToolDefinition, ToolError};
use serde_json::{Value, json};

pub(crate) struct HostInfo;

#[async_trait]
impl Tool for HostInfo {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "host_info",
            "Read live host OS, architecture and working directory without running a shell. Includes Linux distribution/version when os-release is available; missing metadata is omitted.",
            json!({"type":"object","properties":{},"additionalProperties":false}),
        )
    }
    async fn execute(&self, arguments: Value) -> Result<Value, ToolError> {
        if arguments.as_object().is_none_or(|a| !a.is_empty()) {
            return Err(ToolError::new("host_info requires an empty object"));
        }
        let info = json!({
            "os": std::env::consts::OS,
            "architecture": std::env::consts::ARCH,
            "working_directory": std::env::current_dir().map_err(|e| ToolError::new(e.to_string()))?,
        });
        #[cfg(target_os = "linux")]
        let mut info = info;
        #[cfg(target_os = "linux")]
        if let Some((distribution, source)) = linux_distribution() {
            info["distribution"] = distribution;
            info["distribution_source"] = json!(source);
        }
        // Other platforms retain generic fields rather than guessing a product/version.
        Ok(info)
    }
}

#[cfg(target_os = "linux")]
fn linux_distribution() -> Option<(Value, &'static str)> {
    // /etc takes precedence; do not merge vendor data with local overrides.
    for path in ["/etc/os-release", "/usr/lib/os-release"] {
        if let Some(contents) = read_release(std::path::Path::new(path)) {
            let distribution = parse_os_release(&contents);
            return (!distribution.as_object()?.is_empty()).then_some((distribution, path));
        }
    }
    None
}

#[cfg(any(target_os = "linux", test))]
fn read_release(path: &std::path::Path) -> Option<String> {
    use std::io::Read;
    const MAX_BYTES: u64 = 64 * 1024;
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut contents = String::new();
    file.take(MAX_BYTES + 1)
        .read_to_string(&mut contents)
        .ok()?;
    (contents.len() as u64 <= MAX_BYTES).then_some(contents)
}

#[cfg(any(target_os = "linux", test))]
fn parse_os_release(contents: &str) -> Value {
    let mut fields = serde_json::Map::new();
    for line in contents.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let field = match key {
            "NAME" => "name",
            "PRETTY_NAME" => "pretty_name",
            "ID" => "id",
            "VERSION" => "version",
            "VERSION_ID" => "version_id",
            _ => continue,
        };
        if let Some(value) = release_value(value) {
            fields.insert(field.into(), json!(value));
        }
    }
    Value::Object(fields)
}

#[cfg(any(target_os = "linux", test))]
fn release_value(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let quote = value.chars().next()?;
    if quote != '\'' && quote != '"' {
        return value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
            .then(|| value.to_owned());
    }
    let inner = value.strip_prefix(quote)?.strip_suffix(quote)?;
    let mut result = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == quote || c.is_control() {
            return None;
        }
        if c == '\\' && quote == '"' {
            let next = chars.next()?;
            if next.is_control() {
                return None;
            }
            if !"\"\\$`".contains(next) {
                result.push('\\');
            }
            result.push(next);
        } else {
            result.push(c);
        }
    }
    (!result.is_empty()).then_some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_distribution_fixture_without_confusing_version_id_with_version() {
        assert_eq!(
            parse_os_release(
                "# fixture\nNAME=\"Zorin OS\"\nID=zorin\nPRETTY_NAME='Zorin OS 18.1'\nVERSION=18.1\nVERSION_ID=18\nHOME_URL=https://example.test\n"
            ),
            json!({
                "name":"Zorin OS", "id":"zorin", "pretty_name":"Zorin OS 18.1", "version":"18.1", "version_id":"18"
            })
        );
    }

    #[test]
    fn parses_escapes_as_data_without_shell_expansion() {
        assert_eq!(
            parse_os_release(
                r#"NAME="Example \"OS\""
VERSION="$(do-not-run) ${SECRET} `do-not-run` \\ \$ \`"
ID=example
ID=replacement
"#
            ),
            json!({"name":"Example \"OS\"", "version":"$(do-not-run) ${SECRET} `do-not-run` \\ $ `", "id":"replacement"})
        );
    }

    #[test]
    fn malformed_empty_and_unknown_fields_do_not_invent_metadata() {
        assert_eq!(
            parse_os_release(
                "NAME=\"unterminated\nID=bad value\nVERSION_ID=\nSECRET=hidden\nVERSION='a'\"b\"\n"
            ),
            json!({})
        );
        assert_eq!(parse_os_release(""), json!({}));
        assert_eq!(parse_os_release("NAME=\"bad\\\tvalue\""), json!({}));
    }

    #[test]
    fn release_reads_are_bounded_regular_utf8_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("os-release");
        assert!(read_release(&path).is_none());
        assert!(read_release(dir.path()).is_none());
        std::fs::write(&path, "NAME=fixture\n").unwrap();
        assert_eq!(read_release(&path).as_deref(), Some("NAME=fixture\n"));
        std::fs::write(&path, vec![b'x'; 65537]).unwrap();
        assert!(read_release(&path).is_none());
        std::fs::write(&path, [0xff]).unwrap();
        assert!(read_release(&path).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn release_reads_accept_regular_symlink_targets() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("vendor-release");
        let link = dir.path().join("os-release");
        std::fs::write(&target, "ID=fixture").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert_eq!(read_release(&link).as_deref(), Some("ID=fixture"));
    }
}
