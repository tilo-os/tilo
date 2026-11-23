//! Reading a bundle's designated requirement from `codesign`'s output.

/// The designated requirement in `codesign --display --requirements -` output. An explicit one is
/// printed as `designated => …`; the implicit one every bundle signed without its own requirement
/// has, ad hoc or Developer ID, is printed as a comment, `# designated => …`.
#[must_use]
pub fn designated(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let line = line.trim();
        line.strip_prefix("# ")
            .unwrap_or(line)
            .strip_prefix("designated => ")
            .filter(|requirement| !requirement.is_empty())
            .map(str::to_owned)
    })
}

#[cfg(test)]
mod tests {
    use super::designated;

    /// `designated` over `codesign` output: `(row, output, the requirement)`. Each row was a test
    /// of its own, and a failure names its row and the output.
    #[test]
    fn the_designated_requirement_is_read_from_codesign_output() {
        let developer_id = "identifier \"so.tilo.runtime\" and anchor apple generic and certificate leaf[subject.OU] = ABCDE12345";
        let rows = [
            (
                "explicit_and_implicit_requirements_are_both_read",
                format!("# designated => {developer_id}\n"),
                Some(developer_id),
            ),
            (
                "explicit_and_implicit_requirements_are_both_read",
                format!("designated => {developer_id}\n"),
                Some(developer_id),
            ),
            (
                "explicit_and_implicit_requirements_are_both_read",
                "# designated => cdhash H\"331f\"\n".to_owned(),
                Some("cdhash H\"331f\""),
            ),
            (
                "output_without_a_requirement_has_none",
                "Executable=/Applications/Tilo.app\n".to_owned(),
                None,
            ),
            (
                "output_without_a_requirement_has_none",
                "# designated => \n".to_owned(),
                None,
            ),
        ];
        for (row, output, expected) in rows {
            assert_eq!(
                designated(&output).as_deref(),
                expected,
                "{row}: {output:?}"
            );
        }
    }
}
