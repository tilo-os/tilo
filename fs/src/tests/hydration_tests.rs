#[cfg(test)]
mod tests {
    use crate::hydration::Hydrator;
    use iroh_blobs::Hash;
    use iroh::endpoint::Endpoint;
    use iroh_base::EndpointAddr;

    #[tokio::test]
    async fn test_hydration_fetch_simulated() {
        let endpoint = Endpoint::builder().bind().await.unwrap();
        let hydrator = Hydrator::new(endpoint);

        let hash = Hash::from([1u8; 32]);
        let peer_id = iroh::SecretKey::from_bytes(&[2u8; 32]).public();
        let peer_addr = EndpointAddr::new(peer_id);

        let result = hydrator.fetch_range(hash, peer_addr, 0, 100).await.unwrap();
        
        // Confirm it includes peer info and hash
        let s = String::from_utf8_lossy(&result);
        assert!(s.contains("Iroh data"));
        assert!(s.contains(&peer_id.to_string()));
        println!("Verified hydration logic: {}", s);
    }

    #[tokio::test]
    async fn test_hydration_local_simulated() {
        let endpoint = Endpoint::builder().bind().await.unwrap();
        let my_id = endpoint.secret_key().public();
        let hydrator = Hydrator::new(endpoint);

        let hash = Hash::from([3u8; 32]);
        let my_addr = EndpointAddr::new(my_id);

        let result = hydrator.fetch_range(hash, my_addr, 0, 100).await.unwrap();
        let s = String::from_utf8_lossy(&result);
        assert!(s.contains("Local simulated data"));
        println!("Verified local hydration logic: {}", s);
    }
}
