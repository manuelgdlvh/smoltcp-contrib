use std::{alloc::Layout, io, mem::ManuallyDrop};

pub struct Umem<'a> {
    base_addr: usize,
    pages: Box<[ManuallyDrop<UmemPage<'a>>]>,
    alignment: usize,
    free_page_id: Option<u16>,
}

impl<'a> Drop for Umem<'a> {
    fn drop(&mut self) {
        let layout =
            Layout::from_size_align(self.alignment * self.pages.len(), self.alignment.into())
                .expect("Alignment and Size are always valid");
        unsafe {
            std::alloc::dealloc(self.base_addr as *mut u8, layout);
        }
    }
}

impl<'a> Umem<'a> {
    pub fn new(config: Config) -> io::Result<Self> {
        let layout = Layout::from_size_align(
            config.entries * usize::from(config.alignment),
            config.alignment.into(),
        )
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Entries or Alignment are wrong",
            )
        })?;

        // SAFETY: Umem alloc is not null nor uninitialized. Also all inner values are interpretable as [0;N]
        let umem_ptr = unsafe {
            let ptr = std::alloc::alloc(layout);
            if ptr.is_null() {
                return Err(io::Error::last_os_error());
            }
            std::ptr::write_bytes(ptr, 0, usize::from(config.alignment) * config.entries);
            ptr
        };

        let mut pages = Vec::with_capacity(config.entries);
        // Free Pages Initialization
        for i in 0..config.entries {
            let mut page = unsafe {
                let page_ptr = umem_ptr.add(i * usize::from(config.alignment));
                UmemPage::from(page_ptr, config.alignment.into())
            };

            let free_page_id = if i == config.entries - 1 {
                None
            } else {
                Some((i + 1) as u16)
            };

            page.headroom_mut().set_free_page_id(free_page_id);
            pages.push(ManuallyDrop::new(page));
        }
        Ok(Self {
            base_addr: umem_ptr.addr(),
            pages: pages.into_boxed_slice(),
            alignment: config.alignment.into(),
            free_page_id: Some(0),
        })
    }

    pub fn base_addr(&self) -> usize {
        self.base_addr
    }

    pub fn size(&self) -> usize {
        self.pages.len()
    }

    pub fn alignment(&self) -> usize {
        self.alignment
    }

    pub fn read(&self, page_id: usize) -> &UmemPage<'_> {
        &self.pages[page_id]
    }

    fn read_mut<'b>(&'b mut self, page_id: usize) -> &'b mut UmemPage<'a>
    where
        'a: 'b,
    {
        &mut self.pages[page_id]
    }

    pub fn page_id_from(&self, desc: libc::xdp_desc) -> usize {
        desc.addr as usize / self.alignment
    }

    fn desc_addr_from(&self, page_id: usize) -> usize {
        (page_id * self.alignment) + std::mem::size_of::<HeadRoom>()
    }

    pub fn free(&mut self, page_id: usize) -> libc::xdp_desc {
        let last_free_page_id = self.free_page_id;
        let page = self.read_mut(page_id);
        if last_free_page_id.is_some() {
            page.headroom_mut().set_free_page_id(last_free_page_id);
        }

        self.free_page_id = Some(page_id as u16);

        libc::xdp_desc {
            addr: self.desc_addr_from(page_id) as u64,
            len: (self.alignment - std::mem::size_of::<HeadRoom>()) as u32,
            options: 0,
        }
    }

    pub fn write(&mut self, buf: &[u8]) -> io::Result<libc::xdp_desc> {
        let (page, id) = if let Some(val) = self.free_page_id {
            (self.read_mut(val as usize), val)
        } else {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "No free page available",
            ));
        };

        let next_free_page_id = page.headroom().free_page_id();
        page.headroom_mut().set_free_page_id(None);
        page.write_packet(buf);

        self.free_page_id = next_free_page_id;

        Ok(libc::xdp_desc {
            addr: self.desc_addr_from(id as usize) as u64,
            len: buf.len() as u32,
            options: 0,
        })
    }

    pub fn packet_descriptors(&self) -> Vec<libc::xdp_desc> {
        (0..self.pages.len())
            .map(|page_id| libc::xdp_desc {
                addr: self.desc_addr_from(page_id) as u64,
                len: (self.alignment - std::mem::size_of::<HeadRoom>()) as u32,
                options: 0,
            })
            .collect()
    }
}

pub struct UmemPage<'a> {
    // Exclusive access from one userspace Thread. No interaction with the kernel.
    h: &'a mut HeadRoom,
    // Shared read-write access from userspace and kernel.
    buffer: *mut [u8],
}

impl UmemPage<'_> {
    unsafe fn from(ptr: *mut u8, len: usize) -> Self {
        let h = unsafe {
            (ptr as *mut HeadRoom)
                .as_mut()
                .expect("Always initialized with valid memory type")
        };
        let ptr = unsafe { ptr.add(std::mem::size_of::<HeadRoom>()) };
        let len = len - std::mem::size_of::<HeadRoom>();
        let buffer = std::ptr::slice_from_raw_parts_mut(ptr, len);
        Self { h, buffer }
    }

    fn buffer(&self) -> &[u8] {
        // SAFETY: UmemPage lives as long as Umem.
        unsafe { self.buffer.as_ref().unwrap_unchecked() }
    }

    pub fn read_packet(&self, desc: libc::xdp_desc) -> &[u8] {
        let umem_page_len = std::mem::size_of::<HeadRoom>() + self.buffer.len();
        let offset = (desc.addr as usize % umem_page_len) - std::mem::size_of::<HeadRoom>();
        &self.buffer()[offset..offset + desc.len as usize]
    }

    pub fn write_packet(&mut self, buf: &[u8]) {
        // SAFETY: UmemPage lives as long as Umem.
        unsafe {
            self.buffer.as_mut().unwrap_unchecked()[..buf.len()].copy_from_slice(buf);
        }
    }

    pub fn headroom(&self) -> &HeadRoom {
        self.h
    }

    pub fn headroom_mut(&mut self) -> &mut HeadRoom {
        self.h
    }
}

pub struct HeadRoom {
    free_page_id: u16,
}

impl HeadRoom {
    pub fn free_page_id(&self) -> Option<u16> {
        if self.free_page_id == u16::MAX {
            None
        } else {
            Some(self.free_page_id)
        }
    }

    pub fn set_free_page_id(&mut self, page_id: Option<u16>) {
        self.free_page_id = page_id.unwrap_or(u16::MAX);
    }
}

#[derive(Copy, Clone)]
pub struct Config {
    pub entries: usize,
    pub alignment: ChunkAlignment,
}

#[derive(Copy, Clone)]
pub enum ChunkAlignment {
    TwoK,
    FourK,
}

impl From<ChunkAlignment> for usize {
    fn from(value: ChunkAlignment) -> Self {
        match value {
            ChunkAlignment::TwoK => 2048,
            ChunkAlignment::FourK => 4096,
        }
    }
}
