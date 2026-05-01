use supabased_proto::supabased::BranchInfo;

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
            if !branch.status.is_empty() {
                out.push_str(&format!(" ({})", branch.status));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn branch(name: &str, project: &str, status: &str) -> BranchInfo {
        BranchInfo {
            branch_name: name.to_string(),
            project_name: project.to_string(),
            status: status.to_string(),
            created_at: String::new(),
        }
    }

    #[test]
    fn multiple_projects() {
        let branches = vec![
            branch("feat-a", "staging", "ACTIVE_HEALTHY"),
            branch("experiment", "staging", "CREATING_PROJECT"),
            branch("hotfix", "production", "ACTIVE_HEALTHY"),
        ];
        let output = render_branch_tree(&branches);
        assert_eq!(
            output,
            "production\n└── hotfix (ACTIVE_HEALTHY)\n\nstaging\n├── feat-a (ACTIVE_HEALTHY)\n└── experiment (CREATING_PROJECT)"
        );
    }

    #[test]
    fn single_project() {
        let branches = vec![branch("my-branch", "staging", "ACTIVE_HEALTHY")];
        let output = render_branch_tree(&branches);
        assert_eq!(output, "staging\n└── my-branch (ACTIVE_HEALTHY)");
    }

    #[test]
    fn empty_branches() {
        let output = render_branch_tree(&[]);
        assert_eq!(output, "No branches found.");
    }

    #[test]
    fn branch_without_status() {
        let branches = vec![branch("my-branch", "staging", "")];
        let output = render_branch_tree(&branches);
        assert_eq!(output, "staging\n└── my-branch");
    }
}
