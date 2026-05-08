use supabased_proto::supabased::{BranchInfo, BranchOwnership};

/// Render branches grouped by project as a tree diagram.
/// Projects with no branches appear as bare headings.
pub fn render_branch_tree(branches: &[BranchInfo]) -> String {
    use std::collections::BTreeMap;

    let mut by_project: BTreeMap<&str, Vec<&BranchInfo>> = BTreeMap::new();
    for b in branches {
        by_project.entry(&b.project_name).or_default().push(b);
    }

    if by_project.is_empty() {
        return "No branches found.".to_string();
    }

    let mut out = String::new();
    let project_count = by_project.len();
    for (i, (project, branches)) in by_project.iter().enumerate() {
        out.push_str(project);
        out.push('\n');

        for (j, branch) in branches.iter().enumerate() {
            let connector = if j == branches.len() - 1 {
                "└── "
            } else {
                "├── "
            };
            out.push_str(connector);
            out.push_str(&branch.branch_name);
            let ownership = BranchOwnership::try_from(branch.ownership)
                .ok()
                .and_then(ownership_label);
            match (branch.status.is_empty(), ownership) {
                (false, Some(label)) => out.push_str(&format!(" ({}, {label})", branch.status)),
                (false, None) => out.push_str(&format!(" ({})", branch.status)),
                (true, Some(label)) => out.push_str(&format!(" ({label})")),
                (true, None) => {}
            }
            out.push('\n');
        }

        if i < project_count - 1 {
            out.push('\n');
        }
    }

    // Remove trailing newline
    if out.ends_with('\n') {
        out.pop();
    }

    out
}

fn ownership_label(ownership: BranchOwnership) -> Option<&'static str> {
    match ownership {
        BranchOwnership::Yours => Some("YOURS"),
        BranchOwnership::Other => Some("OTHER"),
        BranchOwnership::Untracked => Some("UNTRACKED"),
        BranchOwnership::Unspecified => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn branch(name: &str, project: &str, status: &str, ownership: BranchOwnership) -> BranchInfo {
        BranchInfo {
            branch_name: name.to_string(),
            project_name: project.to_string(),
            status: status.to_string(),
            created_at: String::new(),
            ownership: ownership as i32,
        }
    }

    #[test]
    fn multiple_projects() {
        let branches = vec![
            branch(
                "feat-a",
                "staging",
                "ACTIVE_HEALTHY",
                BranchOwnership::Yours,
            ),
            branch(
                "experiment",
                "staging",
                "CREATING_PROJECT",
                BranchOwnership::Other,
            ),
            branch(
                "hotfix",
                "production",
                "ACTIVE_HEALTHY",
                BranchOwnership::Untracked,
            ),
        ];
        let output = render_branch_tree(&branches);
        assert_eq!(
            output,
            "production\n└── hotfix (ACTIVE_HEALTHY, UNTRACKED)\n\nstaging\n├── feat-a (ACTIVE_HEALTHY, YOURS)\n└── experiment (CREATING_PROJECT, OTHER)"
        );
    }

    #[test]
    fn single_project() {
        let branches = vec![branch(
            "my-branch",
            "staging",
            "ACTIVE_HEALTHY",
            BranchOwnership::Yours,
        )];
        let output = render_branch_tree(&branches);
        assert_eq!(output, "staging\n└── my-branch (ACTIVE_HEALTHY, YOURS)");
    }

    #[test]
    fn empty_branches() {
        let output = render_branch_tree(&[]);
        assert_eq!(output, "No branches found.");
    }

    #[test]
    fn branch_without_status() {
        let branches = vec![branch(
            "my-branch",
            "staging",
            "",
            BranchOwnership::Untracked,
        )];
        let output = render_branch_tree(&branches);
        assert_eq!(output, "staging\n└── my-branch (UNTRACKED)");
    }

    #[test]
    fn unspecified_ownership_is_omitted() {
        let branches = vec![branch(
            "my-branch",
            "staging",
            "ACTIVE_HEALTHY",
            BranchOwnership::Unspecified,
        )];
        let output = render_branch_tree(&branches);
        assert_eq!(output, "staging\n└── my-branch (ACTIVE_HEALTHY)");
    }
}
