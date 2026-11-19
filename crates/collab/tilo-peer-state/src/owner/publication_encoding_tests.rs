//! How ownership's writes read and write as the owner's files hold them.
use super::*;

fn entry(bytes: Vec<u8>) -> StoredOutgoing {
    StoredOutgoing::sample(7, bytes)
}

tilo_test_env::scenario! {
    fn a_publication_entry_keeps_its_class_and_its_bytes_in_json;

    phase an_entry_leaves_as_the_class_it_was_published_by;
    // Ownership leaves each entry as the class it was published by: a write
    // appends after the record's checkpoint, while a member's sealed
    // compaction replaces that author's retained history with itself.
    {
        let delta = entry(Vec::new());
        assert_eq!(NativeOutgoingEntry(&delta).retention(), Retention::Delta);
        let checkpoint = StoredOutgoing {
            incremental: false,
            ..entry(Vec::new())
        };
        assert_eq!(
            NativeOutgoingEntry(&checkpoint).retention(),
            Retention::Checkpoint
        );
    }

    phase outgoing_bytes_are_text_in_json_and_legacy_arrays_still_read;
    // An entry's bytes are base64 text in JSON, an entry written as the
    // legacy array of numbers still reads back the same, and a binary
    // encoding keeps its unchanged sequence.
    {
        let stored = entry((0..=255).collect());
        let json = serde_json::to_value(&stored).unwrap();
        assert!(json["bytes"].is_string(), "{json}");
        assert_eq!(
            serde_json::from_value::<StoredOutgoing>(json).unwrap(),
            stored
        );
        let mut legacy = serde_json::to_value(&stored).unwrap();
        legacy["bytes"] = serde_json::to_value(&stored.bytes).unwrap();
        assert!(legacy["bytes"].is_array());
        assert_eq!(
            serde_json::from_value::<StoredOutgoing>(legacy).unwrap(),
            stored
        );
        let pretty = serde_json::to_vec_pretty(&stored).unwrap();
        assert_eq!(
            serde_json::from_slice::<StoredOutgoing>(&pretty).unwrap(),
            stored
        );
        let binary = postcard::to_allocvec(&stored).unwrap();
        assert_eq!(
            binary,
            postcard::to_allocvec(&(
                stored.ordinal,
                stored.id,
                &stored.bytes,
                &stored.binding,
                stored.timestamp_ns,
                stored.channel,
                stored.incremental
            ))
            .unwrap(),
            "the binary encoding is unchanged"
        );
        assert_eq!(
            postcard::from_bytes::<StoredOutgoing>(&binary).unwrap(),
            stored
        );
    }

    phase a_projected_record_still_reads_and_is_written_without_its_projection;
    // A record an earlier build kept recovery projections in still reads, and
    // this build writes it back without them: it sends the signed originals.
    {
        let projected = entry(vec![1, 2, 3]);
        let mut stored = serde_json::to_value(StoredPublication::default()).unwrap();
        stored["projection"] = serde_json::to_value([&projected]).unwrap();
        let mut read: StoredPublication = serde_json::from_value(stored).unwrap();
        assert_eq!(read.projection, vec![projected]);
        // What a record read from disk does with one (`file_store`).
        read.projection.clear();
        let written = serde_json::to_value(&read).unwrap();
        assert!(
            written.get("projection").is_none(),
            "a record written now carries no projection: {written}"
        );
    }

    phase outgoing_bytes_past_the_cap_are_refused_as_text_and_as_arrays;
    // Bytes past the cap are refused in either JSON form, text before it is
    // decoded.
    {
        let read = |json: &str| read_bytes(&mut serde_json::Deserializer::from_str(json), 4);
        assert_eq!(read(r#""AAECAw==""#).unwrap(), [0, 1, 2, 3]);
        assert_eq!(read("[0, 1, 2, 3]").unwrap(), [0, 1, 2, 3]);
        assert!(read(r#""AAECAwQ=""#).is_err(), "five bytes");
        assert!(
            read(r#""AAECAwQFBg==""#).is_err(),
            "text longer than four bytes'"
        );
        assert!(read("[0, 1, 2, 3, 4]").is_err());
        assert!(read(r#""not base64""#).is_err());
        let fits = entry(vec![0; MAX_BYTES]);
        assert_eq!(
            serde_json::from_value::<StoredOutgoing>(serde_json::to_value(&fits).unwrap()).unwrap(),
            fits
        );
        let past = entry(vec![0; MAX_BYTES + 1]);
        assert!(
            serde_json::from_value::<StoredOutgoing>(serde_json::to_value(&past).unwrap()).is_err()
        );
    }
}
