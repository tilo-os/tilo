//! Retained Btrfs resize ordering: filesystem first on shrink, GPT first on grow.
//! The elevated transaction supplies reattested I/O and durable journal updates.

use super::platform::IoResult;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub partition_guid: String,
    pub filesystem_uuid: String,
    pub device_id: u64,
    pub mount_id: u64,
    pub first_lba: u64,
    pub sector_bytes: u64,
}

#[derive(Clone, Debug)]
pub struct Facts {
    pub binding: Binding,
    pub partition_bytes: u64,
    pub kernel_partition_bytes: u64,
    pub filesystem_bytes: u64,
    pub minimum_bytes: u64,
    pub single_direct_device: bool,
    pub mounted_writable: bool,
    pub exclusive_operation_clear: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    ShrinkFilesystem,
    ShrinkPartition,
    GrowPartition,
    GrowFilesystem,
}

/// Implemented only within the existing elevated staging helper. Every
/// observation must use its retained mount/device descriptors and disk lock.
/// `record_intent` and `record_done` are durable canonical Record updates.
/// A partition update includes disk sync, exact GPT readback, a targeted
/// kernel partition-size update and kernel readback before it returns.
pub trait Volume {
    fn observe(&mut self) -> IoResult<Facts>;
    fn record_intent(&mut self, phase: Phase, bytes: u64) -> IoResult<()>;
    fn record_done(&mut self, phase: Phase, bytes: u64) -> IoResult<()>;
    fn resize_filesystem(&mut self, bytes: u64) -> IoResult<()>;
    fn resize_partition(&mut self, bytes: u64) -> IoResult<()>;
}

fn attest(volume: &mut dyn Volume, binding: &Binding) -> IoResult<Facts> {
    let facts = volume.observe()?;
    if facts.binding != *binding
        || !facts.single_direct_device
        || !facts.mounted_writable
        || !facts.exclusive_operation_clear
        || !matches!(binding.sector_bytes, 512 | 4096)
        || facts.filesystem_bytes > facts.partition_bytes
    {
        return Err("retained Btrfs resize binding changed".into());
    }
    Ok(facts)
}

/// Supports only original, filesystem-shrunk and fully-shrunk states. A
/// failed filesystem resize may leave an intermediate size; that is refused
/// for automatic retry and requires the existing explicit Remove recovery.
/// Cancellation never guesses rollback and never interrupts a GPT mutation.
pub fn shrink(
    volume: &mut dyn Volume,
    binding: &Binding,
    from: u64,
    to: u64,
    cancelled: &dyn Fn() -> bool,
) -> IoResult<()> {
    if !matches!(binding.sector_bytes, 512 | 4096)
        || to == 0
        || to >= from
        || !to.is_multiple_of(1 << 20)
        || !from.is_multiple_of(binding.sector_bytes)
    {
        return Err("invalid retained Btrfs resize extent".into());
    }
    let mut facts = attest(volume, binding)?;
    if facts.minimum_bytes > to {
        return Err("retained Btrfs shrink bound changed".into());
    }
    if ![from, to].contains(&facts.partition_bytes)
        || ![from, to].contains(&facts.filesystem_bytes)
        || ![from, to].contains(&facts.kernel_partition_bytes)
    {
        return Err("retained Btrfs resize requires explicit recovery".into());
    }
    if facts.partition_bytes == to && facts.filesystem_bytes != to {
        return Err("retained Btrfs partition was shortened before its filesystem".into());
    }
    if facts.filesystem_bytes == from {
        if cancelled() {
            return Err("retained Btrfs resize cancelled".into());
        }
        if facts.minimum_bytes > to {
            return Err("retained Btrfs shrink bound changed".into());
        }
        volume.record_intent(Phase::ShrinkFilesystem, to)?;
        volume.resize_filesystem(to)?;
        facts = attest(volume, binding)?;
        if facts.filesystem_bytes != to || facts.partition_bytes != from {
            return Err("retained Btrfs filesystem shrink readback failed".into());
        }
        volume.record_done(Phase::ShrinkFilesystem, to)?;
    }
    if cancelled() {
        return Err("retained Btrfs resize cancelled".into());
    }
    if facts.partition_bytes != to || facts.kernel_partition_bytes != to {
        volume.record_intent(Phase::ShrinkPartition, to)?;
        volume.resize_partition(to)?;
        facts = attest(volume, binding)?;
        if facts.partition_bytes != to
            || facts.kernel_partition_bytes != to
            || facts.filesystem_bytes != to
        {
            return Err("retained Btrfs partition shrink readback failed".into());
        }
        volume.record_done(Phase::ShrinkPartition, to)?;
    }
    Ok(())
}

/// Called by canonical Remove only after its owned staging partitions and
/// firmware entry have been removed and its original free extent reattested.
/// Never enlarge the filesystem before both GPT and kernel device sizes.
pub fn restore(volume: &mut dyn Volume, binding: &Binding, from: u64, to: u64) -> IoResult<()> {
    let mut facts = attest(volume, binding)?;
    if ![from, to].contains(&facts.partition_bytes)
        || ![from, to].contains(&facts.kernel_partition_bytes)
        || facts.filesystem_bytes < to
        || facts.filesystem_bytes > from
    {
        return Err("retained Btrfs restore extent changed".into());
    }
    if facts.partition_bytes != from || facts.kernel_partition_bytes != from {
        volume.record_intent(Phase::GrowPartition, from)?;
        volume.resize_partition(from)?;
        facts = attest(volume, binding)?;
        if facts.partition_bytes != from || facts.kernel_partition_bytes != from {
            return Err("retained Btrfs partition restore readback failed".into());
        }
        volume.record_done(Phase::GrowPartition, from)?;
    }
    if facts.filesystem_bytes != from {
        volume.record_intent(Phase::GrowFilesystem, from)?;
        volume.resize_filesystem(from)?;
        if attest(volume, binding)?.filesystem_bytes != from {
            return Err("retained Btrfs filesystem restore readback failed".into());
        }
        volume.record_done(Phase::GrowFilesystem, from)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        facts: Facts,
        phases: Vec<Phase>,
        fail_partition: bool,
    }
    impl Volume for Fixture {
        fn observe(&mut self) -> IoResult<Facts> {
            Ok(self.facts.clone())
        }
        fn record_intent(&mut self, phase: Phase, _: u64) -> IoResult<()> {
            self.phases.push(phase);
            Ok(())
        }
        fn record_done(&mut self, _: Phase, _: u64) -> IoResult<()> {
            Ok(())
        }
        fn resize_filesystem(&mut self, bytes: u64) -> IoResult<()> {
            assert!(bytes <= self.facts.kernel_partition_bytes);
            self.facts.filesystem_bytes = bytes;
            Ok(())
        }
        fn resize_partition(&mut self, bytes: u64) -> IoResult<()> {
            assert!(self.facts.filesystem_bytes <= bytes);
            if self.fail_partition {
                return Err("injected partition refusal".into());
            }
            self.facts.partition_bytes = bytes;
            self.facts.kernel_partition_bytes = bytes;
            Ok(())
        }
    }
    fn fixture() -> Fixture {
        Fixture {
            facts: Facts {
                binding: Binding {
                    partition_guid: "retained".into(),
                    filesystem_uuid: "existing-files".into(),
                    device_id: 1,
                    mount_id: 7,
                    first_lba: 2048,
                    sector_bytes: 512,
                },
                partition_bytes: 128 << 20,
                kernel_partition_bytes: 128 << 20,
                filesystem_bytes: 128 << 20,
                minimum_bytes: 32 << 20,
                single_direct_device: true,
                mounted_writable: true,
                exclusive_operation_clear: true,
            },
            phases: vec![],
            fail_partition: false,
        }
    }
    #[test]
    fn original_filesystem_shrinks_before_partition_and_restores_after_partition() {
        let mut volume = fixture();
        let binding = volume.facts.binding.clone();
        shrink(&mut volume, &binding, 128 << 20, 64 << 20, &|| false).unwrap();
        restore(&mut volume, &binding, 128 << 20, 64 << 20).unwrap();
        assert_eq!(
            volume.phases,
            vec![
                Phase::ShrinkFilesystem,
                Phase::ShrinkPartition,
                Phase::GrowPartition,
                Phase::GrowFilesystem
            ]
        );
    }
    #[test]
    fn failed_gpt_step_resumes_without_repeating_filesystem_resize() {
        let mut volume = fixture();
        let binding = volume.facts.binding.clone();
        volume.fail_partition = true;
        assert!(shrink(&mut volume, &binding, 128 << 20, 64 << 20, &|| false).is_err());
        assert_eq!(volume.facts.partition_bytes, 128 << 20);
        assert_eq!(volume.facts.filesystem_bytes, 64 << 20);
        volume.fail_partition = false;
        shrink(&mut volume, &binding, 128 << 20, 64 << 20, &|| false).unwrap();
        assert_eq!(
            volume
                .phases
                .iter()
                .filter(|phase| **phase == Phase::ShrinkFilesystem)
                .count(),
            1
        );
    }
    #[test]
    fn stale_minimum_binding_and_unsupported_layouts_refuse_before_any_mutation() {
        for mode in 0..3 {
            let mut volume = fixture();
            let binding = volume.facts.binding.clone();
            match mode {
                0 => volume.facts.minimum_bytes = 65 << 20,
                1 => volume.facts.binding.device_id = 2,
                _ => volume.facts.single_direct_device = false,
            };
            assert!(shrink(&mut volume, &binding, 128 << 20, 64 << 20, &|| false).is_err());
            assert!(volume.phases.is_empty());
        }
    }
}
