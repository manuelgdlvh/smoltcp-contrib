use crate::phy::xdp::rings::Type;
use crate::phy::xdp::umem::{HeadRoom, Umem};
use std::ffi::CString;
use std::os::unix::io::{AsRawFd, RawFd};
use std::{io, mem};

pub struct XdpSocketDesc {
    lower: libc::c_int,
    mtu: usize,
    ifindex: u32,
}

impl AsRawFd for XdpSocketDesc {
    fn as_raw_fd(&self) -> RawFd {
        self.lower
    }
}

impl XdpSocketDesc {
    pub fn new(name: &str) -> io::Result<XdpSocketDesc> {
        let lower = unsafe {
            let lower = libc::socket(libc::AF_XDP, libc::SOCK_RAW | libc::SOCK_NONBLOCK, 0);
            if lower == -1 {
                return Err(io::Error::last_os_error());
            }
            lower
        };

        let ifname = CString::new(name)?;
        let ifindex = unsafe { libc::if_nametoindex(ifname.as_ptr()) };
        if ifindex == 0 {
            return Err(io::Error::last_os_error());
        }

        let mtu = unsafe {
            let fd = libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0);
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }

            let mut ifr: libc::ifreq = mem::zeroed();
            let name = CString::new(ifname).unwrap();
            libc::strncpy(ifr.ifr_name.as_mut_ptr(), name.as_ptr(), libc::IFNAMSIZ);

            if libc::ioctl(fd, libc::SIOCGIFMTU, &mut ifr) < 0 {
                libc::close(fd);
                return Err(io::Error::last_os_error());
            }

            libc::close(fd);
            ifr.ifr_ifru.ifru_mtu
        } as usize;

        Ok(XdpSocketDesc {
            lower,
            mtu,
            ifindex,
        })
    }

    pub fn mtu(&self) -> usize {
        self.mtu
    }

    pub fn ifindex(&self) -> u32 {
        self.ifindex
    }

    pub fn bind_interface(&mut self, queue_id: u32) -> io::Result<()> {
        let sockaddr = libc::sockaddr_xdp {
            sxdp_family: libc::AF_XDP as u16,
            sxdp_flags: 0,
            sxdp_ifindex: self.ifindex(),
            sxdp_queue_id: queue_id,
            sxdp_shared_umem_fd: 0,
        };

        unsafe {
            let res = libc::bind(
                self.lower,
                &sockaddr as *const libc::sockaddr_xdp as *const libc::sockaddr,
                mem::size_of::<libc::sockaddr_xdp>() as libc::socklen_t,
            );
            if res == -1 {
                return Err(io::Error::last_os_error());
            }
        }

        Ok(())
    }

    pub fn bind_umem(&self, umem: &Umem) -> io::Result<()> {
        let config = libc::xdp_umem_reg_v1 {
            addr: umem.base_addr() as u64,
            len: (umem.size() * umem.alignment()) as u64,
            chunk_size: umem.alignment() as u32,
            headroom: std::mem::size_of::<HeadRoom>() as u32,
        };

        let result = unsafe {
            libc::setsockopt(
                self.lower,
                libc::SOL_XDP,
                libc::XDP_UMEM_REG,
                &config as *const _ as *const _,
                std::mem::size_of::<libc::xdp_umem_reg_v1>() as libc::socklen_t,
            )
        };

        if result == -1 {
            return Err(io::Error::last_os_error());
        }

        Ok(())
    }

    pub fn bind_ring(&self, type_: Type, size: usize) -> io::Result<()> {
        let result = unsafe {
            libc::setsockopt(
                self.lower,
                libc::SOL_XDP,
                type_.id(),
                &size as *const _ as *const _,
                mem::size_of_val(&size) as u32,
            )
        };

        if result == -1 {
            return Err(io::Error::last_os_error());
        }

        Ok(())
    }
}

impl Drop for XdpSocketDesc {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.lower);
        }
    }
}
