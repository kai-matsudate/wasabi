// std ではなく core の alloc を使う
extern crate alloc;

use crate::result::Result;
use crate::uefi::EfiMemoryDescriptor;
use crate::uefi::EfiMemoryType;
use crate::uefi::MemoryMapHolder;
use alloc::alloc::GlobalAlloc;
use alloc::alloc::Layout;
// ヒープ領域でのメモリ確保
use alloc::boxed::Box;
// 異なる型の可変借用を実装する trait
use core::borrow::BorrowMut;
// 所有権を実行時に強制する構造体
use core::cell::RefCell;
use core::cmp::max;
use core::fmt;
use core::mem::size_of;
// RefCell によって提供されるRefMutのようなスマートポインタ型から &mut T を取得するための trait
use core::ops::DerefMut;
use core::ptr::null_mut;


// v を2の累乗に切り上げる
pub fn round_up_to_nearest_pow2(v: usize) -> Result<usize> {
    /// v から 1 を引く
    /// ビット表現で先頭に続く 0 の数を数える(最上位ビットの1の位置を取得)
    /// それ以下のビット(全体のビット数から最上位ビットの位置を引いたもの)分だけビットシフトする))
    1usize
        .checked_shl(usize::BITS - v.wrapping_sub(1).leading_zeros())
        .ok_or("Out of range")
}

struct Header {
    next_header: Option<Box<Header>>,
    size: usize,
    is_allocated: bool,
    _reserved: usize,
}

const HEADER_SIZE: usize = size_of::<Header>();
#[allow(clippy::assertions_on_constants)]
const _: () = assert!(HEADER_SIZE == 32);
// Size of Header should be power of 2
const _:() = assert!(HEADER_SIZE.count_ones() == 1);

pub const LAYOUT_PAGE_4K: Layout = unsafe { Layout::from_size_align_unchecked(4096, 4096) };

impl Header {
    fn can_provide(&self, size: usize, align: usize) -> bool {
        // this check is rough - actual size needed may be smaller.
        // HEADER_SIZE * 2 => one for allocated region, another for padding.
        self.size >= size + HEADER_SIZE * 2 + align
    }

    fn is_allocated(&self) -> bool {
        self.is_allocated
    }

    fn end_addr(&self) -> usize {
        self as *const Header as usize + self.size
    }

    unsafe fn new_from_addr(addr: usize) -> Box<Header> {
        let header = addr as *mut Header;
        header.write(Header {
            next_header: None,
            size: 0,
            is_allocated: false,
            _reserved: 0,
        });
        Box::from_raw(addr as *mut Header)
    }

    unsafe fn from_allocated_region(addr: *mut u8) -> Box<Header> {
        let header = addr.sub(HEADER_SIZE) as *mut Header;
        Box::from_raw(header)
    }

    //
    // Note: std::alloc::Layout doc says:
    // > All layouts have an associated size and power-of-two alignment.
    fn provide(&mut self, size: usize, align: usize) -> Option<*mut u8> {
        let size = max(round_up_to_nearest_pow2(size).ok()?, HEADER_SIZE);
        let align = max(align, HEADER_SIZE);

        if self.is_allocated() || !self.can_provide(size, align) {
            None
        } else {
            // Each char represents 32-byte chunks.
            // header_for_allocated.end_addr() self has enough space to allocate the requested object.

            // Make a Header for the allocated object.
            let mut size_used = 0;
            // 確保するメモリ領域の先頭アドレス
            let allocated_addr = (self.end_addr() - size) & !(align - 1);
            // 確保するメモリ領域の直前にヘッダ用のメモリ領域を確保
            let mut header_for_allocated = unsafe { Self::new_from_addr(allocated_addr - HEADER_SIZE) };
            header_for_allocated.size = size + HEADER_SIZE;
            size_used += header_for_allocated.size;
            // リストのつなぎ直し
            header_for_allocated.next_header = self.next_header.take();
            if header_for_allocated.end_addr() != self.end_addr() {
                // Make a Header for padding.
                // 余っている領域をリストに追加
                let mut header_for_padding = unsafe { Self::new_from_addr(header_for_allocated.end_addr()) };
                header_for_padding.is_allocated = false;
                header_for_padding.size = self.end_addr() - header_for_allocated.end_addr();
                size_used += header_for_padding.size;
                // 確保領域 -> パディング領域 とつなぎ直す
                header_for_padding.next_header = header_for_allocated.next_header.take();
                header_for_allocated.next_header = Some(header_for_padding);
            }

            // Shrink self
            assert!(self.size >= size_used + HEADER_SIZE);
            self.size -= size_used;
            self.next_header = Some(header_for_allocated);
            Some(allocated_addr as *mut u8)
        }
    }
}

// 自前実装の allocator 外でヘッダーが開放されないようにする
impl Drop for Header {
    fn drop(&mut self) {
        panic!("Header should not be dropped");
    }
}

impl fmt::Debug for Header {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Header @ {:#018X} {{ size: {:018X}, is_allocated: {} }}",
            self as *const Header as usize,
            self.size,
            self.is_allocated()
        )
    }
}

pub struct FirstFitAllocator {
    // 先頭のヘッダアドレスのみ保持
    first_header: RefCell<Option<Box<Header>>>,
}

#[global_allocator]
pub static ALLOCATOR: FirstFitAllocator = FirstFitAllocator {
    first_header: RefCell::new(None),
};

unsafe impl Sync for FirstFitAllocator {}

unsafe impl GlobalAlloc for FirstFitAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.alloc_with_options(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        let mut region = Header::from_allocated_region(ptr);
        region.is_allocated = false;
        Box::leak(region);
        // region is leaked here to avoid dropping the free info on the memory.
    }
}

impl FirstFitAllocator {
    pub fn alloc_with_options(&self, layout: Layout) -> *mut u8 {
        let mut header = self.first_header.borrow_mut();
        let mut header = header.deref_mut();

        // 先頭から provide できるヘッダを探す
        loop {
            match header {
                Some(e) => match e.provide(layout.size(), layout.align()) {
                    Some(p) => break p,
                    None => {
                        header = e.next_header.borrow_mut();
                        continue;
                    }
                },
                None => {
                    break null_mut::<u8>();
                }
            }
        }
    }

    // EFI のメモリマップから CONVENTIONAL_MEMORY の領域を探して
    // ヘッダを作成してリストに追加する
    pub fn init_with_mmap(&self, memory_map: &MemoryMapHolder) {
        for e in memory_map.iter() {
            if e.memory_type() != EfiMemoryType::CONVENTIONAL_MEMORY {
                continue;
            }
            self.add_free_from_descriptor(e);
        }
    }

    fn add_free_from_descriptor(&self, desc: &EfiMemoryDescriptor) {
        let mut start_addr = desc.physical_start() as usize;
        let mut size = desc.number_of_pages() as usize * 4096;
        // Make sure the allocator does not include the address 0 as a free area.
        if start_addr == 0 {
            start_addr += 4096;
            size = size.saturating_sub(4096);
        }

        if size <= 4096 {
            return;
        }

        let mut header = unsafe { Header::new_from_addr(start_addr) };
        header.next_header = None;
        header.is_allocated = false;
        header.size = size;
        let mut first_header = self.first_header.borrow_mut();
        let prev_last = first_header.replace(header);
        drop(first_header);
        let mut header = self.first_header.borrow_mut();
        header.as_mut().unwrap().next_header = prev_last;
        // It's okay not to be sorted the headers at this point.
        // since all the regions written in memory maps are not contiguous.
        // so that they can't be merged anyway
    }
}
