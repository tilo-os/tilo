#[cfg(test)]
mod tests {
    use crate::metadata::*;
    use iroh_blobs::Hash;

    #[test]
    fn test_metadata_hierarchy() {
        let mut store = MetadataStore::new();
        
        // 1. Check root
        let root = store.get_node(1).expect("Root should exist");
        assert_eq!(root.name, "/");
        
        // 2. Create directory
        let dir_ino = store.create_dir(1, "my_docs");
        let dir_node = store.get_node(dir_ino).expect("Dir should exist");
        assert_eq!(dir_node.name, "my_docs");
        assert!(matches!(dir_node.kind, NodeKind::Directory));
        
        // 3. Create file in directory
        let dummy_hash = Hash::from([0u8; 32]);
        let file_ino = store.create_file(dir_ino, "paper.pdf", 1024, dummy_hash);
        let file_node = store.get_node(file_ino).expect("File should exist");
        assert_eq!(file_node.name, "paper.pdf");
        assert_eq!(file_node.parent, dir_ino);
        assert_eq!(file_node.size, 1024);
        assert_eq!(file_node.hash.unwrap(), dummy_hash);
        
        // 4. Lookup by name
        let found = store.get_child_by_name(dir_ino, "paper.pdf").expect("Should find by name");
        assert_eq!(found.ino, file_ino);
        
        // 5. Check children listing
        let root_node = store.get_node(1).unwrap();
        assert!(root_node.children.as_ref().unwrap().contains(&dir_ino));
    }

    #[test]
    fn test_access_denied_lookups() {
        let mut store = MetadataStore::new();
        let dir_ino = store.create_dir(1, "private");
        store.create_file(dir_ino, "secret.txt", 10, Hash::from([1u8; 32]));

        // Lookup in wrong parent
        let found = store.get_child_by_name(1, "secret.txt");
        assert!(found.is_none(), "Should not find file in wrong directory");

        // Lookup non-existent
        let found = store.get_child_by_name(dir_ino, "ghost.txt");
        assert!(found.is_none());

        // Lookup with invalid inode
        let found = store.get_node(9999);
        assert!(found.is_none());
    }

    #[test]
    fn test_duplicate_names_in_same_dir() {
        let mut store = MetadataStore::new();
        let ino1 = store.create_file(1, "shared.txt", 0, Hash::from([1u8; 32]));
        let ino2 = store.create_file(1, "shared.txt", 0, Hash::from([2u8; 32]));
        
        // MetadataStore currently allows duplicates if created manually, but FUSE lookups 
        // will return the first one found.
        let found = store.get_child_by_name(1, "shared.txt").unwrap();
        assert!(found.ino == ino1 || found.ino == ino2);
    }

    #[test]
    fn test_inode_stability() {
        let mut store = MetadataStore::new();
        let ino1 = store.create_file(1, "a", 0, Hash::from([1u8; 32]));
        let ino2 = store.create_file(1, "b", 0, Hash::from([2u8; 32]));
        
        assert_ne!(ino1, ino2);
        assert_eq!(store.get_node(ino1).unwrap().name, "a");
        assert_eq!(store.get_node(ino2).unwrap().name, "b");
    }
}
