use std::{
    io,
    marker::PhantomData,
    os::fd::RawFd,
    sync::atomic::{Ordering, fence},
};

use libc::{
    XDP_PGOFF_RX_RING, XDP_PGOFF_TX_RING, XDP_RX_RING, XDP_TX_RING, XDP_UMEM_COMPLETION_RING,
    XDP_UMEM_FILL_RING, XDP_UMEM_PGOFF_COMPLETION_RING, XDP_UMEM_PGOFF_FILL_RING,
};

pub fn offsets(socket_fd: RawFd) -> io::Result<libc::xdp_mmap_offsets_v1> {
    let mut offsets = libc::xdp_mmap_offsets_v1 {
        rx: unsafe { std::mem::zeroed() },
        tx: unsafe { std::mem::zeroed() },
        fr: unsafe { std::mem::zeroed() },
        cr: unsafe { std::mem::zeroed() },
    };
    let mut size = std::mem::size_of::<libc::xdp_mmap_offsets_v1>() as u32;

    let result = unsafe {
        libc::getsockopt(
            socket_fd,
            libc::SOL_XDP,
            libc::XDP_MMAP_OFFSETS,
            &mut offsets as *mut _ as *mut _,
            &mut size as *mut _,
        )
    };

    if result != 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(offsets)
}

pub fn build<K: Marker>(
    socket_fd: RawFd,
    type_: Type,
    ring_offsets: libc::xdp_mmap_offsets_v1,
    size: usize,
) -> io::Result<XdpRing<K>> {
    if !size.is_power_of_two() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Ring size must be power of two",
        ));
    }

    let ring_offset: libc::xdp_ring_offset_v1 = match type_ {
        Type::Tx => ring_offsets.tx,
        Type::Rx => ring_offsets.rx,
        Type::Completion => ring_offsets.cr,
        Type::Fill => ring_offsets.fr,
    };

    let mmap_len = ring_offset.desc as usize + (size * std::mem::size_of::<libc::xdp_desc>());

    let ring_ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            mmap_len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED | libc::MAP_POPULATE,
            socket_fd,
            type_.pg_off(),
        )
    };

    if ring_ptr == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }

    Ok(XdpRing::new(type_, ring_ptr, ring_offset, size))
}

#[derive(Clone, Copy)]
pub enum Type {
    Tx,
    Rx,
    Completion,
    Fill,
}

impl Type {
    pub fn id(&self) -> i32 {
        match self {
            Self::Tx => XDP_TX_RING,
            Self::Rx => XDP_RX_RING,
            Self::Completion => XDP_UMEM_COMPLETION_RING,
            Self::Fill => XDP_UMEM_FILL_RING,
        }
    }

    pub fn pg_off(&self) -> i64 {
        match self {
            Self::Tx => XDP_PGOFF_TX_RING,
            Self::Rx => XDP_PGOFF_RX_RING,
            Self::Completion => XDP_UMEM_PGOFF_COMPLETION_RING as i64,
            Self::Fill => XDP_UMEM_PGOFF_FILL_RING as i64,
        }
    }
}

pub trait Marker {}

impl Marker for Reader {}
impl Marker for Writer {}

pub struct Reader {}
pub struct Writer {}

pub struct XdpRing<K: Marker> {
    type_: Type,
    // Is unsound to be & or &mut because kernel at least read this pointers.
    consumer: *mut u32,
    producer: *mut u32,
    descriptors: *mut libc::xdp_desc,
    mask: u32,
    _marker: PhantomData<K>,
}

impl<K: Marker> XdpRing<K> {
    pub fn new(
        type_: Type,
        base_ptr: *mut libc::c_void,
        offset: libc::xdp_ring_offset_v1,
        size: usize,
    ) -> Self {
        unsafe fn ptr_at<T>(base: *mut u8, offset: usize) -> *mut T {
            unsafe { base.add(offset) as *mut T }
        }

        let producer = unsafe {
            ptr_at::<u32>(base_ptr as *mut u8, offset.producer as usize)
                .as_mut()
                .expect("")
        };
        let consumer = unsafe { ptr_at::<u32>(base_ptr as *mut u8, offset.consumer as usize) };
        let descriptors =
            unsafe { ptr_at::<libc::xdp_desc>(base_ptr as *mut u8, offset.desc as usize) };

        Self {
            type_,
            consumer,
            producer,
            descriptors,
            mask: (size - 1) as u32,
            _marker: Default::default(),
        }
    }

    pub fn size(&self) -> u32 {
        self.mask + 1
    }

    pub fn type_(&self) -> Type {
        self.type_
    }
}

impl XdpRing<Reader> {
    pub fn read(&mut self) -> Option<libc::xdp_desc> {
        let (c, p) = unsafe { (*self.consumer, *self.producer) };
        fence(Ordering::Acquire);
        if c == p {
            return None;
        }

        let idx = c & self.mask;
        let res = unsafe {
            let res = *self.descriptors.add(idx as usize);
            fence(Ordering::Release);
            *self.consumer += 1;
            res
        };

        Some(res)
    }
}

impl XdpRing<Writer> {
    pub fn write(&mut self, desc: libc::xdp_desc) -> io::Result<()> {
        let (c, p) = unsafe { (*self.consumer, *self.producer) };
        fence(Ordering::Acquire);

        if (p - c) > self.mask {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Backpressure detected",
            ));
        }

        let idx = p & self.mask;
        unsafe {
            std::ptr::write(self.descriptors.add(idx as usize), desc);
            fence(Ordering::Release);
            *self.producer += 1;
        }

        Ok(())
    }
}

#[derive(Copy, Clone)]
pub struct Config {
    pub size: usize,
}
