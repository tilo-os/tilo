/// Two hosts on one record's route, each with an app on it: a picture one
/// app hands its host reaches the other app through the other host, in
/// verified pieces, as content its record references.
#[tokio::test(flavor = "current_thread")]
async fn a_picture_one_app_offers_reaches_an_app_on_another_host() {
    use signed_worker::OriginalRoom;
    tokio::task::LocalSet::new()
        .run_until(async {
            let (content_a, content_b, _) = crate::record_content::tests::pair(true);
            let host_a = OriginalRoom::with_record_content(content_a, None);
            let room = tilo_desktop_model::RoomId::parse(&host_a.route.cluster_id);
            let host_b = OriginalRoom::with_record_content(content_b, room);
            assert_eq!(host_a.route, host_b.route, "both hosts hold the one record");
            let (app_a, _) = host_a.join_worker(901).await;
            let (app_b, _) = host_b.join_worker(902).await;

            let picture: Vec<u8> = (0..crate::record_content::PIECE_BYTES * 2 + 5)
                .map(|index| u8::try_from(index % 253).unwrap())
                .collect();
            let hash = tilo_content::hash_bytes(&picture);
            app_a.handle.offer_blob(&hash, &picture).await.unwrap();
            assert!(
                app_a.handle.offer_blob(&hash, b"forged").await.is_err(),
                "a host keeps only bytes that are their address"
            );

            let arrivals = app_b.handle.subscribe_blobs().await.unwrap();
            app_b.handle.want_blob(&hash).await.unwrap();
            let arrived = tokio::time::timeout(tilo_test_env::hang_bound(), arrivals.read())
                .await
                .expect("the picture arrives")
                .unwrap();
            assert_eq!(arrived.hash, hash);
            assert_eq!(arrived.bytes, picture);
        })
        .await;
}

/// An app on a host that keeps no record content is told so, rather than
/// waiting for an answer that never comes.
#[tokio::test(flavor = "current_thread")]
async fn a_host_without_record_content_says_so() {
    use signed_worker::OriginalRoom;
    tokio::task::LocalSet::new()
        .run_until(async {
            let host =
                OriginalRoom::new(tilo_user_identity::DeviceCapabilities::host(), None, None);
            let (app, _) = host.join_worker(901).await;
            assert!(app.handle.want_blob(&"0".repeat(64)).await.is_err());
        })
        .await;
}
