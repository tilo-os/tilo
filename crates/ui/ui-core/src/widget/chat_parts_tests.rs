use super::*;

#[test]
fn reactions_given_elsewhere_read_by_their_emoji_and_the_name_they_were_given_under() {
    let props = ReactionsProps {
        relayed: vec![
            "👍\tBo".to_owned(),
            "👍\tCy".to_owned(),
            ":party:\tDee".to_owned(),
        ],
        ..ReactionsProps::default()
    };
    let (_, _, relayed, ..) = said(props);
    assert_eq!(
        relayed,
        [
            ("👍".to_owned(), vec!["Bo".to_owned(), "Cy".to_owned()]),
            (":party:".to_owned(), vec!["Dee".to_owned()]),
        ]
    );
}
