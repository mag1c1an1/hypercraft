use alloc::string::String;

#[derive(Debug)]
struct VcpuScheduler {}


pub struct Thread {
    name: Option<String>,
    phy_cpu_id: usize,
    entry: fn(),
    data: [u8; 256],
}