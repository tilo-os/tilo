//! Reading a bundle's designated requirement from `codesign`'s output.

/// The designated requirement in `codesign --display --requirements -`
/// output. An explicit one is printed as `designated => …`; the implicit one
/// every bundle signed without its own requirement has, ad hoc or Developer
/// ID, is printed as a comment, `# designated => …`.
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

    #[test]
    fn explicit_and_implicit_requirements_are_both_read() {
        let developer_id = "identifier \"so.tilo.runtime\" and anchor apple generic and certificate leaf[subject.OU] = ABCDE12345";
        assert_eq!(
            designated(&format!("# designated => {developer_id}\n")).as_deref(),
            Some(developer_id)
        );
        assert_eq!(
            designated(&format!("designated => {developer_id}\n")).as_deref(),
            Some(developer_id)
        );
        assert_eq!(
            designated("# designated => cdhash H\"331f\"\n").as_deref(),
            Some("cdhash H\"331f\"")
        );
    }

    #[test]
    fn output_without_a_requirement_has_none() {
        assert_eq!(designated("Executable=/Applications/Tilo.app\n"), None);
        assert_eq!(designated("# designated => \n"), None);
    }
}
