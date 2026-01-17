
#include <linux/bpf.h>
#include <bpf/bpf_helpers.h>

struct {
    __uint(type, BPF_MAP_TYPE_XSKMAP);
    __uint(max_entries, 64);
    __type(key, __u32);
    __type(value, __u32);
    __uint(pinning, 1);
} socket_map SEC(".maps");

SEC("xdp")
int xdp_redirect_prog(struct xdp_md *ctx) {
    __u32 index = ctx->rx_queue_index;
    void *val = bpf_map_lookup_elem(&socket_map, &index);
    __u64 val_int = (__u64)(unsigned long)val;

    if (val) {
        int ret = bpf_redirect_map(&socket_map, index, 0);
        bpf_trace_printk(
            "XDP_REDIRECT queue=%d, ret=%d val=%llu\n",
            sizeof("XDP_REDIRECT queue=%d, ret=%d val=%llu\n"),
            index,
            ret, 
            val_int
        );
        return ret;
    } else {
        bpf_trace_printk(
            "XDP_PASS queue=%d val=NULL\n",
            sizeof("XDP_PASS queue=%d val=NULL\n"),
            index
        );
        return XDP_PASS;
    }
}

char _license[] SEC("license") = "GPL";

