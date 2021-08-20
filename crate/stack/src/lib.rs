//! Stack type for a Task's stack
//! and functions thereof
#![no_std]

use x86_64::structures::paging::{Mapper, Page, RecursivePageTable, Size4KiB};

pub fn alloc_stack(size_in_pages: usize, page_table : RecursivePageTable) {
    
}
