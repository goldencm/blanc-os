//! Physical Frame structures and functionality

use bootloader::boot_info::{MemoryRegion, MemoryRegionKind, MemoryRegions};
use spin::{Mutex, Once};
use x86_64::structures::paging::mapper::MapToError;
use x86_64::{PhysAddr, VirtAddr};

/// A global frame allocator initialized from [init](PhysFrameAllocator::init)
pub static FRAME_ALLOCATOR: Once<PhysFrameAllocatorWrapper> = Once::new();
pub static BYTES_AVAILABLE_RAM: Once<u64> = Once::new();

const BITMAP_START: usize = 0xFFFF_FF00_0000_0000;

/// A structure that holds the usable memory region from BIOS and a corresponding
/// bitmap to track allocated frames with associated functions
pub struct PhysFrameAllocator {
    /// The region of memory marked as usable for OS and User use
    pub usable_memory_region: MemoryRegion,

    /// The region of memory reserved for bit map manipulation for
    /// preserving which frames have been allocated or deallocated
    pub bit_map_region: MemoryRegion,
}

impl PhysFrameAllocator {
    /// This function initializes the physical frame allocator for the system
    ///
    /// We take the usable memory region from the bootloader and divide it into two new regions,
    /// the first being a bit map region that will be able to indicate wether a frame is free or used
    /// in the new usable region. And a new usable region minus the consumed bit map region frames.
    /// This function will not assign a new global frame allocator again once initialized
    pub fn init(memory_regions: &'static MemoryRegions) {
        let mut usable_memory_region = MemoryRegion::empty();
        let mut bit_map_region = MemoryRegion::empty();

        usable_memory_region.kind = MemoryRegionKind::Usable;

        let mut num_bit_map_frames: u64 = 0;

        for memory_region in memory_regions.iter() {
            if memory_region.kind == MemoryRegionKind::Usable {
                // Calculate the number of frames required for the bitmap + 1
                num_bit_map_frames = ((memory_region.end - memory_region.start) >> 24) + 1;

                // Assign the bitmap region
                bit_map_region.start = memory_region.start;
                bit_map_region.end = memory_region.start + (num_bit_map_frames << 12) - 1;

                // Assign the new usable memory region
                usable_memory_region.start = memory_region.start + (num_bit_map_frames << 12);
                usable_memory_region.end = memory_region.end;
            }
        }

        // Identity Map the physical bit map frames to pages
        map_bit_frames(bit_map_region, num_bit_map_frames).unwrap();

        // Init global frame allocator
        FRAME_ALLOCATOR.call_once(|| {
            PhysFrameAllocatorWrapper::new(Mutex::new(PhysFrameAllocator {
                usable_memory_region,
                bit_map_region,
            }))
        });

        BYTES_AVAILABLE_RAM.call_once(|| usable_memory_region.end - usable_memory_region.start);
    }

    /// Get the physical frame from the bitmap index
    fn frame_from_bit_index(&self, index: u64) -> PhysFrame {
        let frame_addr = PhysAddr::new(self.usable_memory_region.start + (index << 12));
        PhysFrame::<Size4KiB>::containing_address(frame_addr)
    }

    /// Allocate a specific frame from an addr alligned to the nearest 4KiB frame, will return none if
    /// frame is in use
    pub fn allocate_frame_nth(&self, start: PhysAddr) -> Option<PhysFrame> {
        let start_frame_addr = start.align_down(Size4KiB::SIZE);
        let usable_addr = start_frame_addr.as_u64() - self.usable_memory_region.start;

        // Get the index of the frame in the bitmap
        let index = usable_addr >> 12;

        let u64_byte = index >> 6;
        let bit_index = index % 64;

        let u64_byte_ptr = (self.bit_map_region.start + (u64_byte << 6)) as *mut u64;

        unsafe {
            if *u64_byte_ptr >> bit_index == 1 {
                *u64_byte_ptr &= !(0 << bit_index);
                Some(PhysFrame::from_start_address(start_frame_addr).unwrap())
            } else {
                None
            }
        }
    }
}

// TODO: CONSIDER TURNING BITMAP HANDLING ROUTINES INTO A STRUCT
use x86_64::structures::paging::{
    FrameAllocator, FrameDeallocator, Mapper, Page, PageSize, PageTableFlags, PhysFrame, Size4KiB,
};

use crate::KERNEL_PAGE_TABLE;

unsafe impl FrameAllocator<Size4KiB> for PhysFrameAllocator {
    /// Allocate the next available frame in the usable memory region.
    /// We navigate the bitmap for a empty bit and return the Physical Frame
    /// if there, else no frames are available and return None
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        let mut bm_ptr = BITMAP_START as *mut u64;
        while bm_ptr
            < (BITMAP_START as u64 + (self.bit_map_region.end - self.bit_map_region.start))
                as *mut u64
        {
            let mut quadword = unsafe { *bm_ptr } as u64;
            let qw_clone = quadword;
            let mut index = 0;

            if quadword != 0xFFFFFFFFFFFFFFFF {
                while index < 64 {
                    if quadword & 1 == 0 {
                        // Usable PhysFrame
                        unsafe { *bm_ptr = qw_clone | (0x01 << index) };
                        // Return the frame containing the physical address of the bit in the bitmap
                        return Some(
                            self.frame_from_bit_index(bm_ptr as u64 - BITMAP_START as u64 + index),
                        );
                    }
                    index += 1;
                    quadword >>= 1;
                }
            }
            // Point to next quadword in the map
            unsafe { bm_ptr = bm_ptr.add(8) };
        }
        None
    }
}

impl FrameDeallocator<Size4KiB> for PhysFrameAllocator {
    /// Deallocate a frame in no longer in use
    ///
    /// This is done by clearing the bit in the bit_map to indicate that this
    /// frame is no longer in use
    ///
    /// # Safety
    /// The user must validate that the frame is no longer in use before
    /// deallocation
    unsafe fn deallocate_frame(&mut self, frame: PhysFrame<Size4KiB>) {
        // Get addr of frame along usable_mem_region
        let usable_addr = frame.start_address().as_u64() - self.usable_memory_region.start;

        // Get the index of the frame in the bitmap
        let index = usable_addr >> 12;

        let u64_byte = index >> 6;
        let bit_index = index % 64;

        let u64_byte_ptr = (BITMAP_START as u64 + (u64_byte << 6)) as *mut u64;
        *u64_byte_ptr &= !(1 << bit_index);
    }
}

/// This function is called during the initialization of the frame allocator to identity map the bit map frames
fn map_bit_frames(
    bit_map_region: MemoryRegion,
    num_bit_map_frames: u64,
) -> Result<(), MapToError<Size4KiB>> {
    // Create pages in the bit map region
    let page_range = {
        let bit_map_start = VirtAddr::new(BITMAP_START as u64);
        let bit_map_end =
            VirtAddr::new(BITMAP_START as u64 + (bit_map_region.end - bit_map_region.start));
        let bit_map_start_page = Page::<Size4KiB>::containing_address(bit_map_start);
        let bit_map_end_page = Page::<Size4KiB>::containing_address(bit_map_end);
        Page::range_inclusive(bit_map_start_page, bit_map_end_page)
    };

    let bm_range = bit_map_region.start..bit_map_region.end;
    let frame_addresses = bm_range.step_by(4096);
    let mut frames =
        frame_addresses.map(|addr| PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(addr)));
    let mut empty_allocator = EmptyFrameAllocator;

    assert_eq!(page_range.count() as u64, num_bit_map_frames);

    for page in page_range {
        let frame = frames.next().ok_or(MapToError::FrameAllocationFailed)?;
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
        unsafe {
            KERNEL_PAGE_TABLE
                .wait()
                .unwrap()
                .lock()
                .map_to(page, frame, flags, &mut empty_allocator)
                .unwrap()
                .flush()
        };
        assert_eq!(
            KERNEL_PAGE_TABLE
                .wait()
                .unwrap()
                .lock()
                .translate_page(page)
                .unwrap(),
            frame
        );
    }
    Ok(())
}

/// Wrapper struct for implementing FrameAllocator traits around the mutex type
pub struct PhysFrameAllocatorWrapper {
    pub inner: Mutex<PhysFrameAllocator>,
}

impl PhysFrameAllocatorWrapper {
    /// Return a new [PhysFrameAllocatorWrapper] object with a mutex wrapped in a PhysFrameAllocator
    pub fn new(inner: Mutex<PhysFrameAllocator>) -> Self {
        Self { inner }
    }
}

/// Wrapper implementation for implementing the FrameAllocator trait
unsafe impl FrameAllocator<Size4KiB> for &PhysFrameAllocatorWrapper {
    /// Obtains mutex lock and calls inner [`Allocate Frame`](PhysFrameAllocator::allocate_frame)
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        self.inner.lock().allocate_frame()
    }
}

/// Wrapper implementation for implementing the FrameDeallocator trait
impl FrameDeallocator<Size4KiB> for &PhysFrameAllocatorWrapper {
    /// Obtains mutex lock and calls inner [`Deallocate Frame`](PhysFrameAllocator::deallocate_frame)
    unsafe fn deallocate_frame(&mut self, frame: PhysFrame<Size4KiB>) {
        self.inner.lock().deallocate_frame(frame)
    }
}

/// Used once for allocating the bitmap frames because the map_to function requires an allocator
/// encase more frame allocations are required for more table entries which is impossible
/// at the early stage of execution
#[doc(hidden)]
pub struct EmptyFrameAllocator;

#[doc(hidden)]
unsafe impl FrameAllocator<Size4KiB> for EmptyFrameAllocator {
    /// Returns None, does not affect any memory state
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        None
    }
}
