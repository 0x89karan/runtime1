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

fn print_table(entries: &[TemplateEntry]) {
    let name_w = entries.iter().map(|e| e.name.len()).max().unwrap_or(4).max(4);
    println!("{:<width$}  {:<6}  SHOWCASES", "NAME", "SOURCE", width = name_w);
    for e in entries {
        let src = match e.source {
            TemplateSource::Repo => "Repo",
            TemplateSource::User => "User",
        };
        println!("{:<width$}  {:<6}  {}", e.name, src, e.showcases, width = name_w);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentd::template::TemplateSource;

    #[test]
    fn list_formats_entry_correctly() {
        let entries = vec![TemplateEntry {
            name: "scout".to_string(),
            description: "Read-only researcher.".to_string(),
            source: TemplateSource::Repo,
            showcases: "read_file, list_dir".to_string(),
        }];
        // Smoke-test: ensure print_table doesn't panic with one entry.
        print_table(&entries);
    }
}
