use runtime_fs::{DirectoryGrant, FsOwner, FsRight, FsSession, NativeGrantAuthority};

pub const MOUNT_A: uuid::Uuid = uuid::Uuid::from_u128(0xa11);
pub const MOUNT_B: uuid::Uuid = uuid::Uuid::from_u128(0xb22);

pub fn files_owner() -> FsOwner {
    FsOwner {
        workspace: "test-room".into(),
        app_id: "so.tilo.file-explorer".into(),
    }
}

pub fn all_task1_rights() -> Vec<FsRight> {
    vec![
        FsRight::Enumerate,
        FsRight::Read,
        FsRight::Create,
        FsRight::Rename,
        FsRight::Copy,
        FsRight::Move,
        FsRight::Trash,
    ]
}

pub fn granted_root() -> (
    tempfile::TempDir,
    NativeGrantAuthority,
    FsSession,
    DirectoryGrant,
) {
    let temp = tempfile::tempdir().unwrap();
    let authority = NativeGrantAuthority::new();
    let owner = runtime_fs::FsOwner {
        workspace: "test-room".into(),
        app_id: "so.tilo.file-explorer".into(),
    };
    let root = authority
        .admit_directory(
            std::fs::File::open(temp.path()).unwrap(),
            owner.clone(),
            vec![
                runtime_fs::FsRight::Enumerate,
                runtime_fs::FsRight::Read,
                runtime_fs::FsRight::Create,
                runtime_fs::FsRight::Rename,
                runtime_fs::FsRight::Copy,
                runtime_fs::FsRight::Move,
                runtime_fs::FsRight::Trash,
            ],
        )
        .unwrap();
    let session = authority
        .bind(owner, MOUNT_A, std::slice::from_ref(&root), &[])
        .unwrap();
    (temp, authority, session, root)
}

pub fn admit(authority: &NativeGrantAuthority, path: &std::path::Path) -> DirectoryGrant {
    authority
        .admit_directory(
            std::fs::File::open(path).unwrap(),
            files_owner(),
            all_task1_rights(),
        )
        .unwrap()
}

pub fn bind(
    authority: &NativeGrantAuthority,
    root: &DirectoryGrant,
    mount: uuid::Uuid,
) -> FsSession {
    authority
        .bind(files_owner(), mount, std::slice::from_ref(root), &[])
        .unwrap()
}
