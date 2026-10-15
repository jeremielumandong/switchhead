//! switchhead's MCP server: read-only git tools over newline-delimited JSON-RPC on stdio,
//! scoped to one repository. The app starts it as `switchhead mcp --repo <path>` for a
//! coding CLI's run (see `switchhead-agents`).

pub mod server;
pub mod tools;

use std::path::Path;

pub use tools::GitTools;

/// Serve the tools for the repository at `path` on stdin / stdout until stdin closes.
pub fn run(path: &Path) -> Result<(), String> {
    let repo = switchhead_git::Repo::open(path).map_err(|e| e.to_string())?;
    let mut host = GitTools::new(repo);
    let stdin = std::io::stdin();
    server::serve(&mut host, stdin.lock(), std::io::stdout().lock()).map_err(|e| e.to_string())
}

/// The repository named by `switchhead mcp --repo <path>` arguments (after the program
/// name), or `None` when these are not MCP arguments.
pub fn repo_arg(args: &[String]) -> Option<&str> {
    match args {
        [mcp, flag, path] if mcp == "mcp" && flag == "--repo" => Some(path),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_only_the_mcp_form() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(repo_arg(&a(&["mcp", "--repo", "/r"])), Some("/r"));
        // A folder named "mcp" still opens as a repository.
        assert_eq!(repo_arg(&a(&["mcp"])), None);
        assert_eq!(repo_arg(&a(&["/r"])), None);
    }
}
