use std::convert::TryFrom;
use std::fmt;

use thiserror::Error;

/// A memory address within system memory. Provides both the raw address and relative address so
/// that devices can report both raw and relative addresses in error messages.
#[derive(Copy, Clone, Debug)]
pub struct Addr {
    /// Raw address, originating from game code.
    raw: u16,

    /// Address relative to the start of a particular memory device.
    relative: u16,
}

impl From<u16> for Addr {
    /// Create a new address from a raw address. No offset is applied, so initially both the raw
    /// and relative addresses are the same.
    fn from(raw: u16) -> Self {
        Addr { raw, relative: raw }
    }
}

impl Addr {
    /// Gets the raw address that this Addr points at.
    pub fn raw(&self) -> u16 {
        self.raw
    }

    /// Gets the address this Addr points at relative to the start of some unspecified device.
    pub fn relative(&self) -> u16 {
        self.relative
    }

    /// Gets the address this Addr points at relative to the start of the (unspecified) device, as
    /// a usize for convenient indexing.
    pub fn index(&self) -> usize {
        self.relative as usize
    }

    /// Gets the current offset.
    pub fn offset(&self) -> u16 {
        self.raw - self.relative
    }

    /// Constructs a new address, offsetting the relative address by the specified amount.
    pub fn offset_by(&self, shift: u16) -> Self {
        assert!(shift <= self.relative, "Attempting to offset with overflow");
        Addr {
            raw: self.raw,
            relative: self.relative - shift,
        }
    }
}

impl fmt::Display for Addr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:x}({:x})", self.raw, self.relative)
    }
}

/// Provides access to system memory.
pub trait MemDevice {
    /// Read the byte at the specified address.
    fn read(&self, addr: Addr) -> u8;

    /// Write the byte at the sepcified address.
    fn write(&mut self, addr: Addr, data: u8);
}

/// Wraps a memory device to make it read-only.
#[repr(transparent)]
#[derive(Copy, Clone, Debug, Default)]
pub struct ReadOnly<M>(M);

impl<M> ReadOnly<M> {
    /// Constructs a ReadOnly memory device that wraps the given underlying memory.
    pub fn new(mem: M) -> Self {
        Self(mem)
    }

    /// Unwraps the inner memory device and returns it. This allows mutable access again.
    pub fn into_inner(self) -> M {
        self.0
    }
}

impl<M: MemDevice> MemDevice for ReadOnly<M> {
    fn read(&self, addr: Addr) -> u8 {
        self.0.read(addr)
    }

    fn write(&mut self, addr: Addr, _value: u8) {
        // Read the address to allow the wrapped device to validate the address range.
        self.0.read(addr);
    }
}

/// A rom which does bounds checks, but contains no actual memory (always returns 0, ignores
/// writes).
pub struct NullRom<const N: usize>;

impl<const N: usize> MemDevice for NullRom<N> {
    fn read(&self, addr: Addr) -> u8 {
        assert!(
            addr.index() < N,
            "Address {}  out of range for {} byte nullrom",
            addr,
            N
        );
        0
    }

    fn write(&mut self, addr: Addr, _value: u8) {
        assert!(
            addr.index() < N,
            "Address {}  out of range for {} byte nullrom",
            addr,
            N
        );
    }
}

/// Rom for the bios, which is swapped out once started.
#[repr(transparent)]
#[derive(Copy, Clone, Debug)]
pub struct BiosRom(ReadOnly<[u8; 0x100]>);

impl BiosRom {
    /// Construct a `BiosRom` from an array of 256 bytes. Does not validate the contents.
    pub fn new(data: [u8; 0x100]) -> Self {
        Self(ReadOnly::new(data))
    }

    /// Constructs a `BiosRom` from a slice of contained bytes. The slice must be exactly 256
    /// bytes. No other validation of the contents is performed.
    // TryFrom/TryInto aren't standard imports, so provide a convenience method that doesn't
    // require a TryFrom/TryInto import for the caller.
    pub fn try_from_slice(data: &[u8]) -> Result<Self, BiosSizeError> {
        Self::try_from(data)
    }
}

impl Default for BiosRom {
    fn default() -> Self {
        Self(ReadOnly::new([0; 0x100]))
    }
}

impl MemDevice for BiosRom {
    fn read(&self, addr: Addr) -> u8 {
        self.0.read(addr)
    }

    fn write(&mut self, addr: Addr, value: u8) {
        self.0.write(addr, value)
    }
}

/// Error when converting a slice to a [`BiosRom`]. Contains the number of bytes of the given
/// slice.
#[derive(Copy, Clone, Debug, Error)]
#[error("Expected exactly 256 bytes, got {0}")]
pub struct BiosSizeError(pub usize);

impl TryFrom<&[u8]> for BiosRom {
    type Error = BiosSizeError;

    fn try_from(data: &[u8]) -> Result<Self, BiosSizeError> {
        if data.len() != 0x100 {
            Err(BiosSizeError(data.len()))
        } else {
            let mut arr = [0; 0x100];
            arr.copy_from_slice(data);
            Ok(Self(ReadOnly::new(arr)))
        }
    }
}

/// Enum of different cartridge types.
#[derive(Clone, Debug)]
pub enum Cartridge {
    /// No cartridge. All reads return 0 and all writes are ignored.
    None,
    /// An [`Mbc1Rom`]. This is boxed so that Cartridge doesn't always take up the full size of an
    /// Mbc1Rom even when it is set to None.
    Mbc1(Box<Mbc1Rom>),
}

impl Default for Cartridge {
    fn default() -> Self {
        Cartridge::None
    }
}

impl MemDevice for Cartridge {
    fn read(&self, addr: Addr) -> u8 {
        match self {
            Cartridge::None => NullRom::<0x10000>.read(addr),
            Cartridge::Mbc1(ref cart) => cart.read(addr),
        }
    }

    fn write(&mut self, addr: Addr, value: u8) {
        match self {
            Cartridge::None => NullRom::<0x10000>.write(addr, value),
            Cartridge::Mbc1(ref mut cart) => cart.write(addr, value),
        }
    }
}

/// Variant 1 of the system ROMs.
/// Note that in the GB, the ROM occupies two memory spaces, one before GPU ram for the ROM
/// portion, and one after for the RAM. In this implementation, this split is ignored, and the two
/// memory spaces are squished together. It is the responsibility of the caller to remap the memory
/// spaces as needed to insert the GPU ram.
#[derive(Clone, Debug)]
pub struct Mbc1Rom {
    /// Set of rom banks loaded from the cartridge. Banks 32, 64, and 96 are unreachable but left
    /// in place for convenient addressing.
    rom_banks: [ReadOnly<[u8; 16384]>; 128],
    /// Set of ram banks on this Mbc1Rom, if any. If none, this will just be zeros.
    ram_banks: [[u8; 8192]; 4],
    /// Whether this cartridge type has external ram support. If not, ram cannot be enabled, and
    /// ram_mode does nothing.
    has_ram: bool,

    // Reigsters:
    /// Whether ram is enabled for reading/writing. Otherwise writes are ignored and reads return
    /// dummy values.
    ram_enable: bool,
    /// Rom bank select. This is the low-order 5 bits (0..5) of the rom bank.
    rom_bank: u8,
    /// Bank set is a 2 bit register that either selects the ram-bank or the high-order 2 bits
    /// (5..7) of the rom bank, depending on the mode register. Note that because these two bits
    /// are shared between rom and ram, if mode is 0, only ram bank 0 is accessible, and if mode is
    /// 1, only rom banks 0..32 are accessible.
    bank_set: u8,
    /// Whether the mode is ram-mode. If set, the bank_set register chooses between ram banks
    /// instead of rom bank-sets.
    ram_mode: bool,
}

impl Mbc1Rom {
    /// Convenient access to the fixed rom bank.
    fn fixed_bank(&self) -> &ReadOnly<[u8; 16384]> {
        &self.rom_banks[0]
    }

    /// Get the currently selected rom bank. This will never be bank 0, 32, 64, or 96.
    fn rom_bank(&self) -> &ReadOnly<[u8; 16384]> {
        let low_order = self.rom_bank;
        // In RAM mode, bank_set is not used.
        let high_order = if self.ram_mode { 0 } else { self.bank_set << 5 };
        let rom = (high_order | low_order) as usize;
        &self.rom_banks[rom]
    }

    /// Gets the currently selected ram bank. Does not check if ram is enabled, but does check
    /// ram_mode to see if it should use bank_set to select the correct address.
    fn ram_bank(&self) -> &[u8; 8192] {
        // In ROM mode, only bank 0 is accessible
        let ram = if self.ram_mode { self.bank_set } else { 0 } as usize;
        &self.ram_banks[ram]
    }

    /// Gets the currently selected ram bank. Does not check if ram is enabled, but does check
    /// ram_mode to see if it should use bank_set to select the correct address.
    fn ram_bank_mut(&mut self) -> &mut [u8; 8192] {
        // In ROM mode, only bank 0 is accessible
        let ram = if self.ram_mode { self.bank_set } else { 0 } as usize;
        &mut self.ram_banks[ram]
    }
}

impl MemDevice for Mbc1Rom {
    fn read(&self, addr: Addr) -> u8 {
        match addr.relative() {
            0..=0x3fff => self.fixed_bank().read(addr),
            0x4000..=0x7fff => self.rom_bank().read(addr.offset_by(0x4000)),
            0x8000..=0x9fff => {
                if self.ram_enable {
                    self.ram_bank().read(addr.offset_by(0x8000))
                } else {
                    0
                }
            }
            _ => panic!("Address {} out of range for Mbc1Rom", addr),
        }
    }

    fn write(&mut self, addr: Addr, value: u8) {
        match addr.relative() {
            0x0000..=0x1fff => {
                // Enable ram if the cartridge actually has ram and if the lower four bits of the
                // value are 0xA.
                self.ram_enable = self.has_ram && (value & 0xF) == 0xA;
            }
            0x2000..=0x3fff => {
                // Set the low-order bits of the rom-bank selection from the lower 5 bits of the
                // provided value. If 0 is provided, raise the value to 1.
                self.rom_bank = (value & 0x1f).max(1);
            }
            0x4000..=0x5fff => {
                // Take the 3 bottom bits as the bank set. These will be applied based on whether
                // the mode is ram mode or rom mode when used.
                self.bank_set = value & 0x3;
            }
            0x6000..=0x7fff => {
                // Change between rom mode and ram mode, if the cartridge has Ram.
                self.ram_mode = self.has_ram && (value & 1) != 0;
            }
            0x8000..=0x9fff => {
                if self.has_ram && self.ram_enable {
                    // Write to ram, if ram exists and is enable.
                    self.ram_bank_mut().write(addr.offset_by(0x8000), value);
                }
            }
            _ => panic!("Address {} out of range for Mbc1Rom", addr),
        }
    }
}

impl<const N: usize> MemDevice for [u8; N] {
    fn read(&self, addr: Addr) -> u8 {
        match self.get(addr.index()) {
            Some(val) => *val,
            None => panic!("Address {}  out of range for {} byte memory array", addr, N),
        }
    }

    fn write(&mut self, addr: Addr, value: u8) {
        match self.get_mut(addr.index()) {
            Some(val) => *val = value,
            None => panic!("Address {}  out of range for {} byte memory array", addr, N),
        }
    }
}

// This makes sure that Box<dyn MemDevice> implements MemDevice (as well as Box<Anything that
// implements MemDevice>).
impl<D: MemDevice + ?Sized> MemDevice for Box<D> {
    fn read(&self, addr: Addr) -> u8 {
        (**self).read(addr)
    }

    fn write(&mut self, addr: Addr, value: u8) {
        (**self).write(addr, value)
    }
}

/// Memory device connecting memory mapped IO.
#[derive(Clone, Debug)]
pub struct MemMappedIo {
    bios_enabled: bool,
}

impl MemMappedIo {
    /// Construct new memory-mapped IO manager.
    pub fn new() -> Self {
        MemMappedIo { bios_enabled: true }
    }

    /// Returns true if bios is enabled.
    pub fn bios_enabled(&self) -> bool {
        self.bios_enabled
    }
}

impl Default for MemMappedIo {
    fn default() -> Self {
        Self::new()
    }
}

impl MemDevice for MemMappedIo {
    fn read(&self, addr: Addr) -> u8 {
        match addr.relative() {
            0x00..=0x4f => 0xff,
            0x50 => self.bios_enabled as u8,
            0x51..=0x7f => 0xff,
            _ => panic!("Address {} out of range for Mem Mapped IO", addr),
        }
    }

    fn write(&mut self, addr: Addr, value: u8) {
        match addr.relative() {
            0x00..=0x4f => {}
            0x50 => {
                if value & 1 != 0 {
                    self.bios_enabled = false;
                }
            }
            0x51..=0x7f => {}
            _ => panic!("Address {} out of range for Mem Mapped IO", addr),
        }
    }
}

/// MemoryDevice which configures the standard memory mapping of the real GameBoy.
#[derive(Clone, Debug)]
pub struct GbMmu {
    /// The bios. Mapped to 0..0x100 while bios is enabled.
    bios: BiosRom,
    /// The inserted cartridge. Mapped to 0..0x8000 (rom) and 0xA000..0xC000 (ram).
    cart: Cartridge,
    /// Video Ram. Mapped to 0x8000..0xA000
    vram: [u8; 0x2000],
    /// Working Ram. Mapped to 0xC000..0xE000 and duplicately mapped at 0xE000..0xFE00.
    wram: [u8; 0x2000],
    /// Spirte info. Mapped to 0xFE00..0xFEA0.
    oam: [u8; 160],
    /// Memory mapped IO. Mapped to 0xff00..FF80.
    io: MemMappedIo,
    /// "Page Zero", memory primarily used for software-hardware interaction. Mapped to
    /// 0xFF80..0x10000
    zram: [u8; 127],
}

impl GbMmu {
    /// Construct a new MMU with the given bios and cartridge.
    /// Panics if the given bios data is not exactly 256 bytes.
    pub fn new(bios: BiosRom, cart: Cartridge) -> GbMmu {
        GbMmu {
            bios,
            cart,
            vram: [0; 0x2000],
            wram: [0; 0x2000],
            oam: [0; 160],
            io: MemMappedIo::new(),
            zram: [0; 127],
        }
    }
}

impl Default for GbMmu {
    fn default() -> Self {
        Self::new(Default::default(), Default::default())
    }
}

impl MemDevice for GbMmu {
    fn read(&self, addr: Addr) -> u8 {
        assert!(
            addr.relative() == addr.raw(),
            "Using Root MMU with offset address {}",
            addr
        );
        // Address guaranteed to be in range since we cover the whole memory space.
        match addr.relative() {
            0x0..=0xff if self.io.bios_enabled() => self.bios.read(addr),
            0x0..=0x7fff => self.cart.read(addr),
            0x8000..=0x9fff => self.vram.read(addr.offset_by(0x8000)),
            // Cartridge ram starts right after cartridge Rom, so the offset used here is the
            // size of vram, since we only want to shift the address by the ammount we skipped in
            // order to splice in the vram.
            0xa000..=0xbfff => self.cart.read(addr.offset_by(0x2000)),
            0xc000..=0xdfff => self.wram.read(addr.offset_by(0xc000)),
            0xe000..=0xfdff => self.wram.read(addr.offset_by(0xe000)),
            0xfe00..=0xfe9f => self.oam.read(addr.offset_by(0xfe00)),
            // Unmapped portion above sprite information, always returns 0.
            0xfea0..=0xfeff => 0,
            0xff00..=0xff7f => self.io.read(addr.offset_by(0xff00)),
            0xff80..=0xfffe => self.zram.read(addr.offset_by(0xff80)),
            // Last byte is un-mapped.
            0xffff => 0,
        }
    }

    fn write(&mut self, addr: Addr, value: u8) {
        assert!(
            addr.relative() == addr.raw(),
            "Using Root MMU with offset address {}",
            addr
        );
        // Address guaranteed to be in range since we cover the whole memory space.
        match addr.relative() {
            0x0..=0xff if self.io.bios_enabled() => self.bios.write(addr, value),
            0x0..=0x7fff => self.cart.write(addr, value),
            0x8000..=0x9fff => self.vram.write(addr.offset_by(0x8000), value),
            // Cartridge ram starts right after cartridge Rom, so the offset used here is the
            // size of vram, since we only want to shift the address by the ammount we skipped in
            // order to splice in the vram.
            0xa000..=0xbfff => self.cart.write(addr.offset_by(0x2000), value),
            0xc000..=0xdfff => self.wram.write(addr.offset_by(0xc000), value),
            0xe000..=0xfdff => self.wram.write(addr.offset_by(0xe000), value),
            0xfe00..=0xfe9f => self.oam.write(addr.offset_by(0xfe00), value),
            // Unmapped portion above sprite information.
            0xfea0..=0xfeff => {}
            0xff00..=0xff7f => self.io.write(addr.offset_by(0xff00), value),
            0xff80..=0xfffe => self.zram.write(addr.offset_by(0xff80), value),
            // Last byte is un-mapped.
            0xffff => {}
        }
    }
}
