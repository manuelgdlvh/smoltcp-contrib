use std::{
    cell::RefCell,
    io,
    os::fd::{AsRawFd, RawFd},
    rc::Rc,
};

use smoltcp::{
    phy::{Device, DeviceCapabilities},
    time::Instant,
};

use crate::phy::{
    sys::xdp::XdpSocketDesc,
    xdp::{
        rings::{Reader, Type, Writer, XdpRing},
        umem::Umem,
    },
};

pub mod rings;
pub mod umem;

pub struct XdpSocket<'a> {
    lower: XdpSocketDesc,
    inner: Rc<RefCell<Inner<'a>>>,
}

impl Drop for XdpSocket<'_> {
    fn drop(&mut self) {
        self.lower.close();
    }
}

struct Inner<'a> {
    umem: Umem<'a>,
    tx: XdpRing<Writer>,
    rx: XdpRing<Reader>,
    cr: XdpRing<Reader>,
    fr: XdpRing<Writer>,
}

impl AsRawFd for XdpSocket<'_> {
    fn as_raw_fd(&self) -> RawFd {
        self.lower.as_raw_fd()
    }
}

#[derive(Copy, Clone)]
pub struct Config {
    pub queue_id: u32,
    pub umem: umem::Config,
    pub tx: rings::Config,
    pub rx: rings::Config,
    pub cr: rings::Config,
    pub fr: rings::Config,
}

impl XdpSocket<'_> {
    /// Creates a raw socket, bound to the interface called `name`.
    ///
    /// This requires superuser privileges or a corresponding capability bit
    /// set on the executable.
    ///
    ///
    pub fn new(name: &str, config: Config) -> io::Result<XdpSocket<'_>> {
        let mut lower = XdpSocketDesc::new(name)?;
        let umem = Umem::new(config.umem)?;

        lower.bind_umem(&umem)?;

        lower.bind_ring(Type::Tx, config.tx.size)?;
        lower.bind_ring(Type::Rx, config.rx.size)?;
        lower.bind_ring(Type::Completion, config.cr.size)?;
        lower.bind_ring(Type::Fill, config.fr.size)?;

        let offsets = rings::offsets(lower.as_raw_fd())?;

        let tx = rings::build::<Writer>(lower.as_raw_fd(), Type::Tx, offsets, config.tx.size)?;
        let rx = rings::build::<Reader>(lower.as_raw_fd(), Type::Rx, offsets, config.rx.size)?;
        let cr =
            rings::build::<Reader>(lower.as_raw_fd(), Type::Completion, offsets, config.cr.size)?;
        let mut fr =
            rings::build::<Writer>(lower.as_raw_fd(), Type::Fill, offsets, config.fr.size)?;

        // Expose free pages to kernel
        for desc in umem.packet_descriptors() {
            let _ = fr.write(desc);
        }

        lower.bind_interface(config.queue_id)?;

        Ok(XdpSocket {
            lower,
            inner: Rc::new(RefCell::new(Inner {
                umem,
                tx,
                rx,
                cr,
                fr,
            })),
        })
    }
}

impl<'a> Device for XdpSocket<'a> {
    type RxToken<'b>
        = RxToken
    where
        Self: 'b;

    type TxToken<'b>
        = TxToken<'a>
    where
        Self: 'b;

    fn capabilities(&self) -> DeviceCapabilities {
        let mtu = self.lower.mtu();
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = mtu;
        caps.medium = smoltcp::phy::Medium::Ethernet;
        caps.max_burst_size = Default::default();
        caps.checksum = Default::default();
        caps
    }

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let mut inner = self.inner.borrow_mut();
        if let Some(desc) = inner.rx.read() {
            let page_id = inner.umem.page_id_from(desc);
            let page = inner.umem.read(page_id);

            let data = page.read_packet(desc).to_vec();

            let desc = inner.umem.free(page_id);
            let _ = inner.fr.write(desc);

            return Some((
                RxToken { buffer: data },
                TxToken {
                    inner: self.inner.clone(),
                },
            ));
        }
        None
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(TxToken {
            inner: self.inner.clone(),
        })
    }
}

#[doc(hidden)]
pub struct RxToken {
    buffer: Vec<u8>,
}

impl smoltcp::phy::RxToken for RxToken {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(&self.buffer[..])
    }
}

#[doc(hidden)]
pub struct TxToken<'a> {
    inner: Rc<RefCell<Inner<'a>>>,
}

impl<'a> smoltcp::phy::TxToken for TxToken<'a> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut inner = self.inner.borrow_mut();
        let mut buffer = vec![0; len];
        let result = f(&mut buffer);

        if let Some(desc) = inner.cr.read() {
            let page_id = inner.umem.page_id_from(desc);
            inner.umem.free(page_id);
        }

        match inner.umem.write(&buffer[..]) {
            Ok(desc) => {
                if inner.tx.write(desc).is_err() {
                    let page_id = inner.umem.page_id_from(desc);
                    inner.umem.free(page_id);
                }
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
            Err(err) => panic!("{}", err),
        }

        result
    }
}
