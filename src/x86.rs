use crate::result::Result;
use core::arch::asm;
use core::fmt;
use core::marker::PhantomData;

pub fn hlt() {
    unsafe { asm!("hlt") }
}

pub fn read_io_port_u8(port: u16) -> u8 {
    let mut data: u8;
    unsafe {
        asm!("in al, dx",
            in("dx") port,
            out("al") data)
    }
    data
}

pub fn write_io_port_u8(port: u16, data: u8) {
    unsafe {
        asm!("out dx, al",
            in("al") data,
            in("dx") port)
    }
}

pub fn busy_loop_hint() {
    unsafe { asm!("pause") }
}

// cr3 レジスタの値を読み出す
pub fn read_cr3() -> *mut PML4 {
    let mut cr3: *mut PML4;

    unsafe {
        asm!("mov rax, cr3",
        out("rax") cr3
    )
    }

    cr3
}

pub const PAGE_SIZE: usize = 4096;
// ページのインデックスを表現する上位ビットマスク
const ATTR_MASK: u64 = 0xFFF;
// page table の entry LSB 8ビットのフラグを管理
const ATTR_PRESENT: u64 = 1 << 0;
const ATTR_WRITABLE: u64 = 1 << 1;
const ATTR_WRITE_THROUGH: u64 = 1 << 3;
const ATTR_CACHE_DISABLE: u64 = 1 << 4;

#[derive(Debug, Copy, Clone)]
#[repr(u64)]
// ビットの組み合わせを attr で表現
pub enum PageAttr {
    NotPresent = 0,
    ReadWritekernel = ATTR_PRESENT | ATTR_WRITABLE,
    ReadWriteIo = ATTR_PRESENT | ATTR_WRITABLE | ATTR_WRITE_THROUGH | ATTR_CACHE_DISABLE,
}

#[derive(Debug, Eq, PartialEq)]
// 規定のテーブルが Huge Table の場合も含めて enum 化
pub enum TranslationResult {
    PageMapped4K { phys: u64},
    PageMapped2M { phys: u64},
    PageMapped1G { phys: u64},
}

#[repr(transparent)]
// ジェネリックパラメータにすることで別の型として表現
pub struct Entry<const LEVEL: usize, const SHIFT: usize, NEXT> {
    value: u64,
    next_type: PhantomData<NEXT>,
}

impl <const LEVEL: usize, const SHIFT: usize, NEXT> Entry <LEVEL, SHIFT, NEXT> {
    fn read_value(&self) -> u64 {
        self.value
    }

    // 特定のビットのみを切り出して属性をチェック
    fn is_presenet(&self) -> bool {
        (self.read_value() & (1 << 0)) != 0
    }

    fn is_writable(&self) -> bool {
        (self.read_value() & (1 << 1)) != 0
    }

    fn is_user(&self) -> bool {
        (self.read_value() & (1 << 2)) != 0
    }

    fn format(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "L{}Entry @ {:#p} {{ {:#018X} {}{}{} ",
            LEVEL,
            self,
            self.read_value(),
            if self.is_presenet() {"p"} else {"N"},
            if self.is_writable() {"W"} else {"R"},
            if self.is_user() {"U"} else {"S"},
        )?;
        write!(f, " }}")
    }

    fn table(&self) -> Result<&NEXT> {
        if self.is_presenet() {
            // 上位ビット側だけ返してページインデックスのみ返す
            Ok(unsafe { &*((self.value & !ATTR_MASK) as *const NEXT)})
        } else {
            Err("Page Not Found")
        }
    }
}

impl<const LEVEL: usize, const SHIFT: usize, NEXT> fmt::Display for Entry<LEVEL, SHIFT, NEXT>
{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.format(f)
    }
}

impl<const LEVEL: usize, const SHIFT: usize, NEXT> fmt::Debug for Entry<LEVEL, SHIFT, NEXT>
{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.format(f)
    }
}

#[repr(align(4096))]
pub struct Table<const LEVEL: usize, const SHIFT: usize, NEXT> {
    entry: [Entry<LEVEL, SHIFT, NEXT>; 512],
}

impl<const LEVEL: usize, const SHIFT: usize, NEXT: core::fmt::Debug> Table<LEVEL, SHIFT, NEXT>
{
    fn format(&self, f: &mut fmt::Formatter) -> fmt::Result {
        writeln!(f, "L{}Table @ {:#p} {{", LEVEL, self)?;
        for i in 0..512 {
            let e = &self.entry[i];
            if !e.is_presenet() {
                continue;
            }
            writeln!(f, " . entry[{:3}] = {:?}", i, e)?;
        }
        writeln!(f, "}}")
    }

    pub fn next_level(&self, index: usize) -> Option<&NEXT> {
        self.entry.get(index).and_then(|e| e.table().ok())
    }
}

impl<const LEVEL: usize, const SHIFT: usize, NEXT: fmt::Debug> fmt::Debug for Table<LEVEL, SHIFT, NEXT>
{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.format(f)
    }
}

// SHIFT -> 各レベルのページインデックスが始めるビットを表現
pub type PT = Table<1, 12, [u8; PAGE_SIZE]>;
pub type PD = Table<2, 21, PT>;
pub type PDPT = Table<3, 30, PD>;
pub type PML4 = Table<4, 39, PDPT>;
