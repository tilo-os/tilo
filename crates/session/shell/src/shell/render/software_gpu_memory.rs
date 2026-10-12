//! Give the shared memory a software GPU freed back to the kernel (TIL-731).
//!
//! Mesa's lavapipe, the Vulkan driver that renders on the CPU, backs every
//! ordinary `VkDeviceMemory` with a range of one memfd per device, named
//! `allocation fd` (`llvmpipe_allocate_memory` in Mesa's `lp_texture.c`).
//! Freeing an allocation unmaps it and returns its range to the driver's
//! address-space allocator, but nothing punches the range out of the file,
//! so its pages stay allocated: shmem charged to the process that first
//! touched them, mapped by nobody, and counted as used RAM. A compositor that
//! drew four windows and closed them kept every page their textures had
//! touched until it exited.
//!
//! [`release_freed_memory`] takes those pages back without knowing where the
//! driver's free ranges are, and without touching memory that is not its
//! own. It allocates device memory through the same `VkDevice` until the
//! driver has to grow the file, which it does only once no free range below
//! the end is large enough: every range it was handed was free, and is its
//! own until it frees it. Each one is mapped and `MADV_REMOVE`d, which
//! punches it out of the memfd, and then everything is freed again. The
//! driver hands out each range under its lock, so another thread allocating
//! at the same time gets a different range, and its memory is never touched.
//!
//! Only lavapipe works like this. On any other driver, and on a lavapipe
//! without the memfd, both calls answer `None` and do nothing.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::MetadataExt;

use ash::vk;
use vello::wgpu;
use vello::wgpu::hal::vulkan;

/// The name Mesa's llvmpipe gives the memfd its device memory lives in.
const ALLOCATION_MEMFD: &str = "/memfd:allocation fd (deleted)";

/// Chunk sizes, largest first: a round of large chunks fills the large free
/// ranges, and each smaller round what the round before left.
const CHUNKS: [u64; 4] = [4 << 20, 256 << 10, 16 << 10, PAGE];

/// The smallest chunk: one page, the driver's allocation granularity.
const PAGE: u64 = 4 << 10;

/// The most allocations one call holds at once, whatever the fragmentation.
const MAX_ALLOCATIONS: usize = 16_384;

/// What [`release_freed_memory`] gave back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Released {
    /// Shmem allocated to the device's memfd before the call, in bytes.
    pub before_bytes: u64,
    /// And after it.
    pub after_bytes: u64,
    /// Device allocations made to cover the free ranges.
    pub allocations: usize,
}

impl Released {
    /// Bytes the kernel got back.
    pub fn bytes(&self) -> u64 {
        self.before_bytes.saturating_sub(self.after_bytes)
    }
}

/// Shmem allocated to `device`'s memory when `device` is lavapipe, in bytes:
/// what it holds of the GPU memory it has ever touched, live or freed.
pub fn device_allocated_bytes(device: &wgpu::Device) -> Option<u64> {
    with_lavapipe(device, |held, memory_type| {
        let memfd = held.memfd(memory_type)?;
        allocated(&memfd)
    })
}

/// Return the pages of `device`'s freed memory to the kernel when `device`
/// is lavapipe; `None` when there is nothing of the kind to release.
///
/// Safe beside other threads using `device`: it only touches memory it
/// allocated itself (see the module documentation). It makes one allocation
/// per 4 MiB freed plus a few per free range, so call it off the frame path.
pub fn release_freed_memory(device: &wgpu::Device) -> Option<Released> {
    with_lavapipe(device, |held, memory_type| {
        let memfd = held.memfd(memory_type)?;
        let before_bytes = allocated(&memfd)?;
        for chunk in CHUNKS {
            held.fill_free_ranges(&memfd, memory_type, chunk);
        }
        let allocations = held.memories.len();
        held.free();
        Some(Released {
            before_bytes,
            after_bytes: allocated(&memfd)?,
            allocations,
        })
    })
}

/// Run `f` with an empty [`Held`] and a host-visible memory type when
/// `device` is lavapipe on Vulkan.
fn with_lavapipe<T>(
    device: &wgpu::Device,
    f: impl FnOnce(&mut Held<'_>, u32) -> Option<T>,
) -> Option<T> {
    // SAFETY: the hal device is used only inside this call, through entry
    // points that need no external synchronisation on the device
    // (vkAllocateMemory, vkFreeMemory) and on memory objects created here.
    let guard = unsafe { device.as_hal::<vulkan::Api>() }?;
    let hal: &vulkan::Device = &guard;
    let memory_type = lavapipe_memory_type(hal)?;
    let mut held = Held {
        device: hal.raw_device(),
        memories: Vec::new(),
    };
    f(&mut held, memory_type)
}

/// A host-visible memory type when `hal` is lavapipe.
fn lavapipe_memory_type(hal: &vulkan::Device) -> Option<u32> {
    let instance = hal.shared_instance().raw_instance();
    let physical = hal.raw_physical_device();
    let mut driver = vk::PhysicalDeviceDriverProperties::default();
    let mut properties = vk::PhysicalDeviceProperties2::default().push_next(&mut driver);
    // SAFETY: `physical` belongs to `instance`, and both outlive the call.
    unsafe { instance.get_physical_device_properties2(physical, &mut properties) };
    if driver.driver_id != vk::DriverId::MESA_LLVMPIPE {
        return None;
    }
    // SAFETY: as above.
    let memory = unsafe { instance.get_physical_device_memory_properties(physical) };
    let count = usize::try_from(memory.memory_type_count).ok()?;
    memory
        .memory_types
        .iter()
        .take(count)
        .position(|ty| {
            ty.property_flags
                .contains(vk::MemoryPropertyFlags::HOST_VISIBLE)
        })
        .and_then(|index| u32::try_from(index).ok())
}

/// Shmem allocated to `memfd`, in bytes.
fn allocated(memfd: &File) -> Option<u64> {
    Some(memfd.metadata().ok()?.blocks().saturating_mul(512))
}

/// The device memory this call allocated; freed when dropped.
struct Held<'a> {
    device: &'a ash::Device,
    memories: Vec<vk::DeviceMemory>,
}

impl Held<'_> {
    /// Allocate `size` bytes, map them and punch them out of the memfd.
    /// Returns the mapping's address.
    fn punch(&mut self, memory_type: u32, size: u64) -> Option<usize> {
        if self.memories.len() >= MAX_ALLOCATIONS {
            return None;
        }
        let length = usize::try_from(size).ok()?;
        let info = vk::MemoryAllocateInfo::default()
            .allocation_size(size)
            .memory_type_index(memory_type);
        // SAFETY: a plain allocation with no extension structures; it is
        // freed in `free`, after every use below.
        let memory = unsafe { self.device.allocate_memory(&info, None) }.ok()?;
        self.memories.push(memory);
        // SAFETY: `memory` was just allocated from a host-visible type and is
        // not mapped yet; the mapping lives until it is freed.
        let address = unsafe {
            self.device
                .map_memory(memory, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty())
        }
        .ok()?;
        // SAFETY: the range is exactly the mapping of an allocation this call
        // owns, and nothing reads it. On lavapipe it is a shared mapping of
        // the driver's memfd, so MADV_REMOVE frees the file's pages there.
        let removed = unsafe { libc::madvise(address, length, libc::MADV_REMOVE) };
        (removed == 0).then_some(address.addr())
    }

    /// The lavapipe memfd this device's memory lives in: the one a fresh
    /// page of it is mapped from.
    fn memfd(&mut self, memory_type: u32) -> Option<File> {
        let address = self.punch(memory_type, PAGE)?;
        memfd_mapped_at(address)
    }

    /// Allocate `chunk`-sized memory until the driver grows `memfd` to place
    /// one: by then every free range of at least `chunk` below its end is
    /// held, and punched.
    fn fill_free_ranges(&mut self, memfd: &File, memory_type: u32, chunk: u64) {
        let Ok(size) = memfd.metadata().map(|meta| meta.size()) else {
            return;
        };
        while self.punch(memory_type, chunk).is_some() {
            if !memfd.metadata().is_ok_and(|meta| meta.size() <= size) {
                return;
            }
        }
    }

    fn free(&mut self) {
        for memory in self.memories.drain(..) {
            // SAFETY: allocated by this call from `self.device`, and nothing
            // refers to it any more. Freeing it unmaps it.
            unsafe { self.device.free_memory(memory, None) };
        }
    }
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        self.free();
    }
}

/// The lavapipe memfd the mapping starting at `address` maps, opened anew so
/// it stays valid whatever the driver does with its own descriptor.
fn memfd_mapped_at(address: usize) -> Option<File> {
    let start = format!("{address:x}-");
    let maps = BufReader::new(File::open("/proc/self/maps").ok()?);
    let inode = maps.lines().map_while(Result::ok).find_map(|line| {
        if !line.starts_with(&start) || !line.ends_with(ALLOCATION_MEMFD) {
            return None;
        }
        line.split_whitespace().nth(4)?.parse::<u64>().ok()
    })?;
    std::fs::read_dir("/proc/self/fd")
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| {
            std::fs::read_link(entry.path())
                .is_ok_and(|target| target.as_os_str() == ALLOCATION_MEMFD)
        })
        .filter_map(|entry| File::open(entry.path()).ok())
        .find(|file| file.metadata().is_ok_and(|meta| meta.ino() == inode))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::gpu::RenderContext;

    const MIB: u64 = 1 << 20;

    /// Map `bytes` of fresh device memory, write all of it, and free it.
    fn touch_and_free(device: &wgpu::Device, bytes: u64) {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("til-731 touched"),
            size: bytes,
            usage: wgpu::BufferUsages::MAP_WRITE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: true,
        });
        let len = usize::try_from(bytes).expect("a buffer this process can map");
        buffer
            .slice(..)
            .get_mapped_range_mut()
            .copy_from_slice(&vec![0x5A; len]);
        buffer.unmap();
        drop(buffer);
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
    }

    /// The release gives back what freed buffers touched, and never what a
    /// live one holds.
    #[test]
    fn freed_device_memory_goes_back_to_the_kernel() {
        let mut context = RenderContext::new();
        let id = pollster::block_on(context.device(None)).expect("a wgpu adapter");
        let device = &context.devices[id].device;
        let Some(start) = release_freed_memory(device) else {
            // Not lavapipe: there is no memfd to read, and nothing to keep.
            assert_eq!(device_allocated_bytes(device), None);
            return;
        };
        let baseline = start.after_bytes;
        // Larger than a suballocation block, so each buffer is an allocation
        // of its own, freed with the buffer.
        for _ in 0..4 {
            touch_and_free(device, 24 * MIB);
        }
        let freed = device_allocated_bytes(device).expect("lavapipe");
        assert!(
            // Each buffer reuses the range the one before freed, give or
            // take the pages the driver's own bookkeeping moves.
            freed >= baseline.saturating_add(23 * MIB),
            "the touched buffers were not charged to the memfd: {baseline} -> {freed}"
        );
        // Live memory with contents, mapped for reading after the release.
        let live = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("til-731 live"),
            size: 8 * MIB,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: true,
        });
        let live_len = usize::try_from(8 * MIB).expect("8 MiB fits");
        live.slice(..)
            .get_mapped_range_mut()
            .copy_from_slice(&vec![0xA5; live_len]);
        let released = release_freed_memory(device).expect("lavapipe");
        assert!(
            // What is left is the live buffer and the staging copy wgpu maps
            // it through at creation, 8 MiB each; the 24 MiB freed is gone.
            released.after_bytes <= baseline.saturating_add(17 * MIB),
            "freed memory stayed allocated: baseline {baseline}, freed {freed}, {released:?}"
        );
        assert!(
            live.slice(..)
                .get_mapped_range()
                .iter()
                .all(|byte| *byte == 0xA5),
            "the release punched a live allocation: {released:?}"
        );
    }
}
