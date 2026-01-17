use std::ffi::CString;
use std::os::unix::io::AsRawFd;

use libbpf_sys::{BPF_ANY, bpf_map_update_elem, bpf_obj_get};
use smoltcp::phy::wait as phy_wait;
use smoltcp::{
    phy::Device,
    phy::RxToken,
    time::Instant,
    wire::{EthernetFrame, PrettyPrinter},
};

use smoltcp_contrib::phy::xdp::{ChunkConfig, Config, RingConfig, UmemConfig, XdpSocket};

// sudo ip link set dev wlan0 xdp obj xdp.o sec xdp
// sudo RUST_BACKTRACE=1 cargo run --example tcpdump-xdp -- {IFNAME}

fn main() {
    let ifname = std::env::args()
        .nth(1)
        .expect("usage: xdp-example <ifname>");

    let config = Config {
        queue_id: 0,
        umem: UmemConfig {
            entries: 1024,
            alignment: ChunkConfig::FourK,
        },
        tx: RingConfig { size: 16 },
        rx: RingConfig { size: 16 },
        cr: RingConfig { size: 16 },
        fr: RingConfig { size: 16 },
    };
    let mut socket: XdpSocket<'_> = XdpSocket::new(ifname.as_str(), config).unwrap();
    let socket_fd = socket.as_raw_fd() as i32;

    let pin_path = CString::new("/sys/fs/bpf/xdp/globals/socket_map").unwrap();
    let map_fd;
    unsafe {
        // Open pinned map
        map_fd = bpf_obj_get(pin_path.as_ptr());
        if map_fd < 0 {
            eprintln!("Failed to open pinned map: {}", map_fd);
            panic!();
        }
    }

    unsafe {
        let ret = bpf_map_update_elem(
            map_fd,
            &config.queue_id as *const _ as *const _,
            &socket_fd as *const _ as *const _,
            BPF_ANY as u64,
        );
        if ret != 0 {
            eprintln!("Failed to update map: {}", ret);
        }
    }

    loop {
        if let Some((rx, _)) = socket.receive(Instant::now()) {
            rx.consume(|buffer| {
                println!(
                    "{}",
                    PrettyPrinter::<EthernetFrame<&[u8]>>::new("", &buffer)
                );
            })
        }

        phy_wait(socket.as_raw_fd(), None).unwrap();
    }
}
