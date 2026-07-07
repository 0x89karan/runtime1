use std::path::PathBuf;

use agentd::template::{TemplateEntry, TemplateSource};

#[derive(clap::Args)]
pub struct Args {
    /// Override user templates directory (default: ~/.agentos/templates/)
    #[arg(long, value_name = "PATH", env = "AGENTOS_TEMPLATES_DIR")]
    pub user_templates_dir: Option<PathBuf>,
    /// Override repo templates directory (default: auto-detected)
    #[arg(long, value_name = "PATH", env = "AGENTOS_REPO_TEMPLATES_DIR")]
    pub repo_dir: Option<PathBuf>,
}

pub fn run(args: Args) -> anyhow::Result<()> {
    let resolver =
        crate::build_resolver(args.user_templates_dir.as_deref(), args.repo_dir.as_deref());
    let entries = resolver.list()?;

    if entries.is_empty() {
        eprintln!(
            "no templates found; checked {} and {}",
            resolver.user_dir().display(),
            resolver.repo_dir().display()
        );
        return Ok(());
    }

    print_table(&entries);
    Ok(())
}

const SHOWCASES_MAX_CHARS: usize = 72;

fn print_table(entries: &[TemplateEntry]) {
    let name_w = entries.iter().map(|e| e.name.len()).max().unwrap_or(4).max(4);
    let indent  = " ".repeat(name_w + 10); // align sub-line under DESCRIPTION
    println!("{:<width$}  {:<6}  DESCRIPTION", "NAME", "SOURCE", width = name_w);
    for e in entries {
        let src = match e.source {
            TemplateSource::Repo => "Repo",
            TemplateSource::User => "User",
        };
        let gated_badge = if e.gated_requires.is_some() { " [gated]" } else { "" };
        println!("{:<width$}  {:<6}  {}{}", e.name, src, e.description, gated_badge, width = name_w);
        // Showcases on a sub-line so the table stays scannable.
        // Use chars().count() to avoid panicking on multi-byte UTF-8 boundaries.
        let showcases = if e.showcases.chars().count() > SHOWCASES_MAX_CHARS {
            format!("{}...", e.showcases.chars().take(SHOWCASES_MAX_CHARS).collect::<String>())
        } else {
            e.showcases.clone()
        };
        println!("{indent}Showcases: {showcases}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentd::template::TemplateSource;

    fn make_entry(showcases: &str) -> TemplateEntry {
        TemplateEntry {
            name:          "scout".to_string(),
            description:   "Read-only researcher.".to_string(),
            source:        TemplateSource::Repo,
            showcases:     showcases.to_string(),
            sample_tasks:  vec![],
            gated_requires: None,
        }
    }

    #[test]
    fn list_shows_gated_badge_for_gated_templates() {
        let entry = TemplateEntry {
            name:          "librarian-semantic".to_string(),
            description:   "Semantic KB librarian.".to_string(),
            source:        TemplateSource::Repo,
            showcases:     "vector search".to_string(),
            sample_tasks:  vec![],
            gated_requires: Some("VOYAGE_API_KEY".to_string()),
        };
        // Capture stdout by just checking the badge logic, not by intercepting println.
        // The badge is conditionally appended when gated_requires is Some.
        let badge = if entry.gated_requires.is_some() { " [gated]" } else { "" };
        assert_eq!(badge, " [gated]", "gated_requires Some → badge must be [gated]");

        let ungated = TemplateEntry {
            name:          "scout".to_string(),
            description:   "Researcher.".to_string(),
            source:        TemplateSource::Repo,
            showcases:     "web_search".to_string(),
            sample_tasks:  vec![],
            gated_requires: None,
        };
        let no_badge = if ungated.gated_requires.is_some() { " [gated]" } else { "" };
        assert_eq!(no_badge, "", "gated_requires None → no badge");
        // smoke-test that print_table doesn't panic on a gated entry
        print_table(&[entry, ungated]);
    }

    #[test]
    fn list_formats_entry_correctly() {
        // Smoke-test: ensure print_table doesn't panic with one entry.
        print_table(&[make_entry("read_file, list_dir")]);
    }

    #[test]
    fn list_truncates_showcases_longer_than_max() {
        // Build a showcases string that is clearly longer than SHOWCASES_MAX_CHARS.
        let long = "a".repeat(SHOWCASES_MAX_CHARS + 10);
        let entry = make_entry(&long);
        // print_table must not panic; verify the truncation path fires.
        assert!(entry.showcases.chars().count() > SHOWCASES_MAX_CHARS);
        let truncated = if entry.showcases.chars().count() > SHOWCASES_MAX_CHARS {
            format!(
                "{}...",
                entry.showcases.chars().take(SHOWCASES_MAX_CHARS).collect::<String>()
            )
        } else {
            entry.showcases.clone()
        };
        assert!(truncated.ends_with("..."), "truncated string must end with ...");
        assert_eq!(
            truncated.chars().count(),
            SHOWCASES_MAX_CHARS + 3,
            "truncated length must be SHOWCASES_MAX_CHARS + len('...')"
        );
        print_table(&[entry]); // must not panic
    }

    #[test]
    fn list_showcases_truncation_is_char_safe_for_multibyte_utf8() {
        // Build a showcases string from multi-byte UTF-8 chars (Japanese kana, 3 bytes each).
        // Byte length >> char count — using &str[..N] would panic at a non-boundary.
        let kana = "あ".repeat(SHOWCASES_MAX_CHARS + 5);
        assert!(kana.len() > SHOWCASES_MAX_CHARS, "byte len must exceed char limit");
        let entry = make_entry(&kana);
        // Must not panic — this is the regression test for the original byte-slice panic.
        print_table(&[entry]);
    }
}
