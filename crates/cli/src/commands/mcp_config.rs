//! Generate paste-ready MCP stdio client configuration.
use crate::commands::shared::json_quote;
use std::path::{Path, PathBuf};

pub(crate) const CLIENTS: &[&str] = &["dsh", "claude", "cursor", "generic"];

pub(crate) fn render(client: &str, binary: &Path, root: &Path) -> Result<String, String> {
    if !CLIENTS.contains(&client) {
        return Err(format!(
            "unknown client: {client} (expected {})",
            CLIENTS.join(", ")
        ));
    }
    let command = json_quote(&binary.display().to_string());
    let root = json_quote(&root.display().to_string());
    let args = format!(r#"["mcp","--root",{root}]"#);
    let text = if client == "generic" {
        format!(
            r#"{{
  "name": "csvx",
  "transport": "stdio",
  "command": {command},
  "args": {args},
  "env": {{}}
}}"#
        )
    } else {
        format!(
            r#"{{
  "mcpServers": {{
    "csvx": {{
      "command": {command},
      "args": {args},
      "env": {{}}
    }}
  }}
}}"#
        )
    };
    Ok(text)
}

pub(crate) fn absolute(path: &Path, fallback: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        fallback.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_uses_a_flat_shape() {
        let text = render("generic", Path::new("/bin/csvx"), Path::new("/data")).unwrap();
        assert!(text.contains(r#""name": "csvx""#));
        assert!(text.contains(r#""transport": "stdio""#));
    }

    #[test]
    fn server_clients_use_mcp_servers() {
        for client in ["dsh", "claude", "cursor"] {
            let text = render(client, Path::new("/bin/csvx"), Path::new("/data")).unwrap();
            assert!(text.contains(r#""mcpServers""#));
            assert!(text.contains(r#""command": "/bin/csvx""#));
            assert!(text.contains(r#""args": ["mcp","--root","/data"]"#));
        }
    }

    #[test]
    fn unknown_client_lists_choices() {
        let error = render("nope", Path::new("/bin/csvx"), Path::new("/data")).unwrap_err();
        assert!(error.contains("unknown client"));
        assert!(error.contains("dsh, claude, cursor, generic"));
    }
}
