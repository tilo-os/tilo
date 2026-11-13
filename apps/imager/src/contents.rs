//! What a drive contains, in words, for its row.

use ui::i18n::ArgValue;

use crate::resource::CATALOG;

pub(crate) fn drive_contents(count: usize, names: &[&str]) -> String {
    if count == 0 {
        return CATALOG.t("imager-drive-empty");
    }
    let count = ArgValue::Number(f64::from(u32::try_from(count).unwrap_or(u32::MAX)));
    if names.is_empty() {
        return CATALOG.t_args("imager-drive-partitions", &[("count", count)]);
    }
    let joined = ui::format::list(
        &names
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>(),
    );
    CATALOG.t_args(
        "imager-drive-partitions-held",
        &[("count", count), ("filesystems", ArgValue::Text(joined))],
    )
}

#[cfg(feature = "destination")]
pub(crate) fn drive_line(first: &str, count: usize, names: &[&str]) -> String {
    ui::format::list(&[first.to_owned(), drive_contents(count, names)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drive_says_what_it_holds_with_the_catalogue_words() {
        assert_eq!(drive_contents(0, &[]), "Empty disk");
        assert_eq!(drive_contents(1, &[]), "1 partition");
        assert_eq!(drive_contents(3, &[]), "3 partitions");
        assert_eq!(
            drive_contents(2, &["ext4", "btrfs"]),
            "2 partitions (ext4, btrfs)"
        );
    }

    #[cfg(feature = "destination")]
    #[test]
    fn a_drive_line_joins_its_device_and_its_contents() {
        assert_eq!(
            drive_line("/dev/sdb", 1, &["vfat"]),
            "/dev/sdb, 1 partition (vfat)"
        );
    }
}
