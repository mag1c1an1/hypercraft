# hypercraft
🚧WIP🚧 hypercraft is a VMM library written in Rust. If you are interested in Design & Implement about this project, please see this [discussion](https://github.com/orgs/rcore-os/discussions/13). Currently, hypercraft relies heavily on the [Arceos](https://github.com/rcore-os/arceos) crate, reusing multiple crates and modules from arceos for development.

## Build & Run

### Riscv Linux
**Clone project**
```
# create workspace
mkdir $(WORKSPACE)
cd $(WORKSPACE)

# clone project
git clone https://github.com/arceos-hypervisor/arceos.git
cd arceos
git checkout hypervisor
git submodule update --init --recursive
```

**Download Disk File & Linux Binary**  

Download disk file from Baidu Cloud Disk to `$(WORKSPACE)/guest/linux`:

链接: https://pan.baidu.com/s/1OGuOjMe0VEFvDhPg3nzSjA?pwd=5ewv 提取码: 5ewv 复制这段内容后打开百度网盘手机App，操作更方便哦 
--来自百度网盘超级会员v2的分享

**Build & Run**
```
# build & run
make ARCH=riscv64 A=apps/hv HV=y LOG=info run
```

### aarch64 nimbos
**Clone project**
```
# create workspace
mkdir $(WORKSPACE)
cd $(WORKSPACE)

# clone project
git clone https://github.com/arceos-hypervisor/arceos.git
cd arceos
git checkout hypervisor
git submodule update --init --recursive
```

**Download NimbOS Image**  
Download nimbos-aarch64.bin in [nimbos](https://drive.google.com/drive/folders/1Hfn6RI0GsNxoAmMQ1Gj1kZdcm_NkhRP0?usp=sharing) from Google Cloud Disk to `$(WORKSPACE)/arceos/apps/hv/guest/nimbos`: 

**Build & Run**
```
# build & run
make ARCH=aarch64 A=apps/hv HV=y LOG=info GUEST=nimbos run
```

### x86_64 nimbos
**Clone project**
```
# create workspace
mkdir $(WORKSPACE)
cd $(WORKSPACE)

# clone project
git clone https://github.com/arceos-hypervisor/arceos.git
cd arceos
git checkout hypervisor
git submodule update --init --recursive
```

**Build NimbOS BIOS**  
Download [nimbos image](https://drive.google.com/file/d/1Q3yNmpnh3pamrhHGZV_uz6wUFGklidGk/view?usp=drive_link) from Google Cloud Disk to `$(WORKSPACE)/arceos/apps/hv/guest/nimbos`: 

```
# build nimbos bios
cd apps/hv/guest/nimbos/bios
make
cp out/rvm-bios.bin ..
```

**Download NimbOS Image**  
Download nimbos-x86.bin from [here](https://drive.google.com/drive/folders/1Hfn6RI0GsNxoAmMQ1Gj1kZdcm_NkhRP0?usp=sharing) to `$(WORKSPACE)/arceos/apps/hv/guest/nimbos` and rename it to `nimbos.bin`: 

**Build & Run**
```
# build & run
make ARCH=x86_64 A=apps/hv HV=y LOG=info GUEST=nimbos run
```

## RoadMap
- CPU Virtualization
    - [x] Vcpu abstract layer(`vcpu_create()`, `vcpu_read()`, `vcpu_write()`, `vcpu_run()`)
    - [x] Load & run hello world binary in example.
    - [x] `PerCpu` struct Design to support SMP.
    - [ ] Mult-Core boot.
    - [ ] Multi-Guest switch support(vcpu schedule)
- Memory Virtualization
    - [x] Nested Page Table Support
    - [ ] Multi-level Page Table Supportd
- I/O Virtualization
    - [x] Device Passthrought Supportd
    - [ ] IOMMU Support
    - [ ] Device Emulate
- Interrupt Virtualization
    - [x] Timer Interrupt Enable
    - [x] PLIC Emulate && Interrupt Inject
    - [ ] AIA Supported
- System Supported
    - [x] rCore-Tutorial-v3
    - [x] Linux
    - [ ] Arceos


## Relevant Issues

- [rcore-os/arceos #41](https://github.com/rcore-os/arceos/issues/41)
- [rcore-os/arceos #39](https://github.com/rcore-os/arceos/issues/39)

## References
- [rivosinc/salus](https://github.com/rivosinc/salus): Risc-V hypervisor for TEE development
- [equation314/RVM-Tutorial](https://github.com/equation314/RVM-Tutorial): Let's write an x86 hypervisor in Rust from scratch!
- [zircon](https://fuchsia.dev/fuchsia-src/concepts/kernel): Zircon is the core platform that powers Fuchsia. Zircon is composed of a kernel (source in /zircon/kernel) as well as a small set of userspace services, drivers, and libraries (source in /zircon/system/) necessary for the system to boot, talk to hardware, load userspace processes and run them, etc. Fuchsia builds a much larger OS on top of this foundation.
- [KuangjuX/hypocaust-2](https://github.com/KuangjuX/hypocaust-2): hypocaust-2, a type-1 hypervisor with H extension run on RISC-V machine

