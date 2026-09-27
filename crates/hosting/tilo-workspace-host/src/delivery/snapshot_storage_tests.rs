use super::*;
#[test]
fn snapshot_storage_owner_binding_rejects_foreign_root_workspace_pool_and_unattached_owner() {
    let (_temp, mut owner) = tests::hosted_owner();
    let (ingress, _rx) = crate::https::ingress_channel();
    let inventory = ingress.host_work();
    assert!(owner.snapshot_directory(&inventory).is_err());
    owner.attach_data_resources(&ingress.host_bytes()).unwrap();
    let directory = owner
        .snapshot_directory(&inventory)
        .expect("actual attached owner must bind its held root");
    let lease = directory.reserve([1; 16]).unwrap();
    owner.validate_snapshot_reservation(&lease).unwrap();
    let (_other_temp, mut other) = tests::hosted_owner();
    other.attach_data_resources(&ingress.host_bytes()).unwrap();
    assert!(other.validate_snapshot_reservation(&lease).is_err());
    let file = std::fs::File::open(owner.root()).unwrap();
    let wrong = inventory
        .snapshot_directory(&file, owner.workspace_id + 1)
        .unwrap()
        .reserve([1; 16])
        .unwrap();
    assert!(owner.validate_snapshot_reservation(&wrong).is_err());
    let (foreign, _rx) = crate::https::ingress_channel();
    assert!(owner.snapshot_directory(&foreign.host_work()).is_err());
    let wrong = foreign
        .host_work()
        .snapshot_directory(&file, owner.workspace_id)
        .unwrap()
        .reserve([1; 16])
        .unwrap();
    assert!(owner.validate_snapshot_reservation(&wrong).is_err());
    lease.remove().unwrap();
}
