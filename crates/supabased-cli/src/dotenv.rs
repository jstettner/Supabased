use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

const BLOCK_START: &str = "# Supabase Configuration";

pub fn format_supabase_block(
    project_name: &str,
    branch_name: &str,
    url: &str,
    publishable_key: &str,
    secret_key: &str,
) -> String {
    format!(
        "{BLOCK_START} - {project_name}/{branch_name}\nSUPABASE_URL={url}\nSUPABASE_PUBLISHABLE_KEY={publishable_key}\nSUPABASE_KEY={publishable_key}\nSUPABASE_SECRET_KEY={secret_key}"
    )
}

/// Update or create a `.env` file with Supabase configuration.
/// If the file exists and contains a `# Supabase Configuration` block,
/// replace it in place. Otherwise, append the block.
pub fn update_dotenv(path: &Path, block: &str) -> Result<(), Box<dyn std::error::Error>> {
    if path.exists() {
        let contents = fs::read_to_string(path)?;
        let new_contents = if let Some(start) = contents.find(BLOCK_START) {
            // Find the end of the block: next blank line or EOF
            let block_end = contents[start..]
                .find("\n\n")
                .map(|pos| start + pos)
                .unwrap_or(contents.len());
            format!("{}{}{}", &contents[..start], block, &contents[block_end..])
        } else {
            // Append with a blank line separator
            if contents.ends_with('\n') {
                format!("{contents}\n{block}\n")
            } else if contents.is_empty() {
                format!("{block}\n")
            } else {
                format!("{contents}\n\n{block}\n")
            }
        };
        fs::write(path, new_contents)?;
    } else {
        fs::write(path, format!("{block}\n"))?;
    }
    restrict_owner_only(path)?;
    Ok(())
}

fn restrict_owner_only(path: &Path) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }

    #[cfg(not(unix))]
    {
        let _ = path;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn format_block() {
        let block = format_supabase_block(
            "staging",
            "my-branch",
            "https://ref-abc.supabase.co",
            "sb_publishable_value",
            "secret-key",
        );
        assert!(block.starts_with("# Supabase Configuration - staging/my-branch"));
        assert!(block.contains("SUPABASE_URL=https://ref-abc.supabase.co"));
        assert!(block.contains("SUPABASE_PUBLISHABLE_KEY=sb_publishable_value"));
        assert!(block.contains("SUPABASE_KEY=sb_publishable_value"));
        assert!(block.contains("SUPABASE_SECRET_KEY=secret-key"));
    }

    #[test]
    fn creates_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        let block = format_supabase_block("s", "b", "url", "key", "secret");
        update_dotenv(&path, &block).unwrap();
        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.starts_with("# Supabase Configuration"));
        assert!(contents.ends_with('\n'));
    }

    #[test]
    fn appends_to_existing_without_supabase_block() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        fs::write(&path, "OTHER_VAR=hello\n").unwrap();
        let block = format_supabase_block("s", "b", "url", "key", "secret");
        update_dotenv(&path, &block).unwrap();
        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.starts_with("OTHER_VAR=hello\n"));
        assert!(contents.contains("SUPABASE_URL=url"));
    }

    #[test]
    fn replaces_existing_block_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        let old = "OTHER=1\n\n# Supabase Configuration - old/old\nSUPABASE_URL=old\nSUPABASE_KEY=old\nSUPABASE_SECRET_KEY=old\n\nANOTHER=2\n";
        fs::write(&path, old).unwrap();
        let block = format_supabase_block("s", "b", "new-url", "new-key", "new-secret");
        update_dotenv(&path, &block).unwrap();
        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.contains("SUPABASE_URL=new-url"));
        assert!(contents.contains("SUPABASE_PUBLISHABLE_KEY=new-key"));
        assert!(contents.contains("SUPABASE_KEY=new-key"));
        assert!(!contents.contains("SUPABASE_URL=old"));
        assert!(contents.contains("OTHER=1"));
        assert!(contents.contains("ANOTHER=2"));
    }
}
